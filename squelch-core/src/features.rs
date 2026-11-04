//! Hidden features: what this mailbox has been let into ahead of everyone else.
//!
//! A FEATURE HERE IS A KEY AND NOTHING ELSE. `calendar` is on for a tenant or it
//! is not; there are no percentages, no variants, and no payloads. The moment a
//! flag needs a value it has stopped being a gate and become configuration,
//! which already has a home ([`crate::config`]).
//!
//! THREE PARTIES, ONE LIST:
//!
//! - THE CONTROL PLANE decides. An operator ticks a box on `/admin/features`
//!   and the row lands in its database (`tenant_features`, in `squelch_control::store`).
//! - THE DAEMON asks, with the same control token it mints invites with, and
//!   keeps the answer in a [`Features`] handle that every subsystem can read.
//! - THE CLIENT is told, on `/client/stats`, and hides what is not listed.
//!
//! OFF IS THE ANSWER TO EVERY DOUBT. A daemon with no control plane, a control
//! plane that is down, a key nobody has heard of, an old daemon that sends no
//! list at all: each reads as "not enabled". A hidden feature that fails to
//! appear costs its one tester a message to the operator; one that appears for
//! everybody is a launch nobody decided on.
//!
//! THE EXCEPTION IS THE OPERATOR'S OWN MACHINE. `SQUELCH_FEATURES` names keys
//! that are on regardless of what any control plane says, which is how a
//! self-host (who has the source anyway) and a development daemon get at a
//! feature with no tenant row to tick. A hosted tenant cannot set a variable on
//! its own pod, so this is not a way around the admin page.

use std::collections::BTreeSet;
use std::sync::{Arc, RwLock};

/// One hidden feature, as the code that gates on it names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Feature {
    /// What is stored, sent, and compared. See [`is_valid_key`].
    pub key: &'static str,
    /// One sentence for the operator reading the admin page, saying what
    /// ticking the box lets somebody see.
    pub summary: &'static str,
}

/// Every hidden feature this build knows about.
///
/// THE ADMIN PAGE IS DRAWN FROM THIS, so a flag exists for an operator the
/// moment the control plane deploys a build that lists it, and stops being
/// offered the moment it is deleted from here (which is what launching a
/// feature is: delete the entry and the checks on it, and everybody has it).
///
/// Adding one is a `pub const` beside this list, an entry in it, and a
/// `features.enabled(features::THE_KEY)` wherever the daemon gates on it. The
/// clients gate on the same key string; a flag only a client reads still gets
/// an entry here, because this list is the only thing the admin page offers.
pub const KNOWN: &[Feature] = &[
    Feature {
        key: CALENDAR,
        summary: "The Calendar page: events from mail, a Google Calendar connection, and creating \
                  and editing events.",
    },
    Feature {
        key: RENOTIFY,
        summary: "Smart re-notify: a second banner for mail that still needs handling, when the \
                  Mac comes online or a meeting ends.",
    },
    Feature {
        key: CALENDAR_OWN,
        summary: "Passband replaces the events Gmail adds to the calendar from mail with its \
                  own, DELETING Gmail's copy, and keeps them current from the mail.",
    },
    Feature {
        key: UNCAPPED_TRIAGE,
        summary: "No daily triage lanes: backfill, re-triage and access checks run the day they \
                  are queued instead of waiting for tomorrow. The 10,000-run daily breaker and \
                  the monthly LLM budget still apply.",
    },
    Feature {
        key: UNDERSTANDING,
        summary: "What Passband knows about you: a nightly model pass writes short entries about \
                  the owner and the people they reply to, triage reads them, and Settings shows \
                  every one.",
    },
];

/// The daemon's background and access lane caps (see
/// [`crate::triage::agent_config::AgentConfig`]) are lifted for this mailbox,
/// so no email waits for tomorrow's budget, and ticking it releases the jobs
/// those caps already parked. The total (`daily_run_cap`) stays as the
/// runaway breaker it is, as do the revisit and archive search sub-caps.
/// Spend is still counted, so the day's numbers stay true. Hosted, it still
/// answers to the tenant's monthly LLM budget at the gateway, which this does
/// not touch. OFF IS THE CAPS AS CONFIGURED, which is every daemon that never
/// heard of the key. docs/TRIAGE-OPERATIONS.md, "Spend and revisit limits".
pub const UNCAPPED_TRIAGE: &str = "uncapped_triage";


/// Deep user understanding (docs/UNDERSTANDING.md §2.3, §5): the nightly
/// profile pass (`triage::profile`), the entries triage reads from it
/// (`triage::memory::StoreMemoryContext`), and the review page's routes
/// (`/client/memory`). OFF IS AS IF IT DID NOT EXIST: the pass does not run
/// and spends nothing, triage is handed no memory lines, and the routes 404.
/// It ships dark because it spends tokens and writes sentences about people,
/// and the first weeks are for reading what the model writes about a handful
/// of tenants before anyone else sees it. Entries already written stay in the
/// store while it is off, unserved.
pub const UNDERSTANDING: &str = "understanding";

/// Smart re-notify (issue #625, docs/RENOTIFY.md): the daemon records the Mac
/// app's connections and, at a moment the owner can act, sends a `renotify`
/// frame naming mail that still needs them. OFF IS AS IF IT DID NOT EXIST: no
/// session is recorded and no frame is sent.
pub const RENOTIFY: &str = "renotify";

/// The Calendar page and everything behind it on the human door: the
/// timeline, the user's word on a ghost, the Google Calendar connection and
/// sync, and the user's own event writes. OFF IS AS IF IT DID NOT EXIST: those
/// routes 404, `/client/stats` says `timeline: false` and carries no
/// `calendar` object, and the sync loop calls Google for nothing. What keeps
/// running regardless is the cheap, local half: ghost projection in the
/// triage commit and correspondent timezone inference, so a mailbox switched
/// on sees its recent history at once (docs/CALENDAR.md section 11).
pub const CALENDAR: &str = "calendar";

/// Passband owns the events mail makes ([`crate::gcal::own`]): after each
/// calendar pass it replaces Gmail's own copies on the user's primary
/// calendar with events of its own, DELETING Gmail's, and keeps them current
/// from the mail. The one place the daemon writes to, and deletes from, a
/// calendar without the user asking for that write, and a deleted event is
/// not ours to restore, so it ships dark and is ticked per tenant (owner's
/// call, 2026-10-30). The key was added and removed inside PR #647 before
/// any build offered it, so no tenant row can predate this.
pub const CALENDAR_OWN: &str = "calendar_own";

/// Keys that were hidden features once and have LAUNCHED (or been dropped),
/// and so must never be handed out again.
///
/// A RETIRED KEY IS NEVER REUSED. Rows ticked for it on the control plane's
/// `tenant_features` table outlive the launch: they are harmless, because no
/// code asks for the key any more, but they are still stored and still served
/// on `GET /tenant/features` and `/client/stats`. A new feature under an old
/// name would arrive already switched on for whoever had the old one.
///
/// Empty today. `calendar` sat here for a few days after it launched for
/// everyone in October 2026; it went back behind its flag on 2026-10-31, under
/// the SAME key on purpose, so the testers who had it ticked before the launch
/// keep it.
pub const RETIRED: &[&str] = &[];

/// The environment variable naming keys that are on for this daemon no matter
/// what. Comma separated; whitespace around a key is ignored.
pub const OVERRIDE_VAR: &str = "SQUELCH_FEATURES";

/// Ceiling on a key. Far above any name worth typing, and present because keys
/// cross two wires and land in a database column.
pub const MAX_KEY_LEN: usize = 48;

/// Ceiling on how many keys one answer may carry. A bound on what a daemon
/// will hold for a control plane it does not fully trust, not a product limit.
pub const MAX_KEYS: usize = 128;

/// Whether `key` is shaped like a feature key: a lowercase letter, then
/// lowercase letters, digits and underscores, at most [`MAX_KEY_LEN`] long.
///
/// A GRAMMAR, NOT A REGISTRY CHECK. The daemon passes on keys it has never
/// heard of, because a flag only the client reads is a real thing and an older
/// daemon must not swallow it. What it will not pass on is a string that could
/// not be a key at all.
pub fn is_valid_key(key: &str) -> bool {
    let mut bytes = key.bytes();
    bytes.next().is_some_and(|b| b.is_ascii_lowercase())
        && key.len() <= MAX_KEY_LEN
        && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// The live set of enabled features for this mailbox.
///
/// CHEAP TO CLONE AND SAFE TO HOLD: every clone reads the same set, so a
/// subsystem takes one at boot and asks it whenever it needs to know. The
/// answer can change between two calls (that is the point), so ask at the
/// moment of the decision rather than caching the boolean.
#[derive(Clone, Default)]
pub struct Features {
    inner: Arc<Inner>,
}

#[derive(Default)]
struct Inner {
    /// On regardless of the control plane. Fixed at construction.
    pinned: BTreeSet<String>,
    /// What the control plane last answered. Empty until it has.
    granted: RwLock<BTreeSet<String>>,
}

impl std::fmt::Debug for Features {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Features").field(&self.list()).finish()
    }
}

impl Features {
    /// A handle with nothing enabled and nothing pinned.
    pub fn none() -> Self {
        Self::default()
    }

    /// A handle with `pinned` on for good. Keys that are not keys are dropped.
    pub fn pinned<I, S>(pinned: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        Self {
            inner: Arc::new(Inner {
                pinned: sanitize(pinned),
                granted: RwLock::default(),
            }),
        }
    }

    /// A handle pinned from [`OVERRIDE_VAR`], which is unset on every hosted
    /// tenant and therefore pins nothing there.
    pub fn from_env() -> Self {
        let raw = std::env::var(OVERRIDE_VAR).unwrap_or_default();
        Self::pinned(raw.split(',').map(str::trim))
    }

    /// Whether `key` is on for this mailbox right now.
    pub fn enabled(&self, key: &str) -> bool {
        self.inner.pinned.contains(key) || self.granted().contains(key)
    }

    /// Everything that is on, sorted, for the wire.
    pub fn list(&self) -> Vec<String> {
        self.inner.pinned.union(&self.granted()).cloned().collect()
    }

    /// Replace what the control plane granted with a fresh answer. Returns
    /// whether anything changed, so the caller can log a flip and stay quiet
    /// about the hundreds of polls that say the same thing.
    pub fn replace_granted<I, S>(&self, keys: I) -> bool
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let next = sanitize(keys);
        let mut granted = self
            .inner
            .granted
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if *granted == next {
            return false;
        }
        *granted = next;
        true
    }

    fn granted(&self) -> std::sync::RwLockReadGuard<'_, BTreeSet<String>> {
        // A poisoned lock still holds a whole set: `replace_granted` swaps it
        // in one assignment and nothing can panic halfway through one.
        self.inner
            .granted
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Keep what is shaped like a key, up to [`MAX_KEYS`] of them.
fn sanitize<I, S>(keys: I) -> BTreeSet<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    keys.into_iter()
        .filter(|k| is_valid_key(k.as_ref()))
        .take(MAX_KEYS)
        .map(|k| k.as_ref().to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_is_lowercase_words_and_nothing_else() {
        for good in ["calendar", "agent_send", "v2", "a"] {
            assert!(is_valid_key(good), "{good}");
        }
        let long = "a".repeat(MAX_KEY_LEN + 1);
        for bad in [
            "",
            "Calendar",
            "2fast",
            "_lead",
            "has space",
            "dash-ed",
            "a,b",
            "<b>",
            long.as_str(),
        ] {
            assert!(!is_valid_key(bad), "{bad:?}");
        }
    }

    #[test]
    fn nothing_is_on_until_something_says_so() {
        let features = Features::none();
        assert!(!features.enabled("calendar"));
        assert!(features.list().is_empty());
    }

    #[test]
    fn a_grant_is_seen_by_every_clone_and_replaced_whole() {
        let features = Features::none();
        let reader = features.clone();
        assert!(features.replace_granted(["calendar", "agent_send"]));
        assert!(reader.enabled("calendar"));
        assert_eq!(reader.list(), ["agent_send", "calendar"]);

        // The same answer again is not a change.
        assert!(!features.replace_granted(["agent_send", "calendar"]));

        // A REPLACEMENT, not a merge: a box unticked is a feature gone.
        assert!(features.replace_granted(["agent_send"]));
        assert!(!reader.enabled("calendar"));
    }

    #[test]
    fn a_pinned_key_survives_a_control_plane_that_grants_nothing() {
        let features = Features::pinned(["calendar", " nope ", ""]);
        assert!(features.enabled("calendar"));
        features.replace_granted(Vec::<String>::new());
        assert!(features.enabled("calendar"));
        assert_eq!(features.list(), ["calendar"]);
    }

    #[test]
    fn an_answer_is_sanitized_before_it_is_believed() {
        let features = Features::none();
        let mut keys: Vec<String> = (0..MAX_KEYS + 10).map(|i| format!("k{i}")).collect();
        keys.push("<script>".into());
        features.replace_granted(&keys);
        let list = features.list();
        assert_eq!(list.len(), MAX_KEYS);
        assert!(list.iter().all(|k| is_valid_key(k)));
    }

    #[test]
    fn every_known_feature_has_a_usable_key_and_no_twin() {
        let mut seen = BTreeSet::new();
        for feature in KNOWN {
            assert!(is_valid_key(feature.key), "{}", feature.key);
            assert!(!feature.summary.is_empty(), "{}", feature.key);
            assert!(seen.insert(feature.key), "{} twice", feature.key);
        }
    }
}
