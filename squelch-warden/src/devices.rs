//! Parsing what `squelchd token first-paired` and `squelchd token activity`
//! printed inside a tenant pod.
//!
//! The activation signal (issue #89) is ONE TIMESTAMP and nothing else. The
//! daemon's side of that contract is one line on stdout — an RFC3339 instant, or
//! the word `none` — and this module is the whole of the warden's side of it.
//!
//! [`crate::pair`] is the neighbour to read first; this is the same shape with a
//! smaller answer. The difference that matters is what the two are parsing. A
//! pairing is a live credential, so nothing there may be logged. This is a
//! timestamp, which is not a secret — but the caller
//! ([`crate::provision::Warden::first_paired`]) still never logs the output, and
//! that is deliberate: the rule "exec output does not go in the log" is cheaper
//! to keep whole than to carve exceptions out of, and the next command someone
//! adds may not be so harmless.
//!
//! REFUSAL IS AN ANSWER HERE. `Option<Option<..>>` is not clever nesting for its
//! own sake: the outer layer is "did the daemon answer the question at all", the
//! inner is "and the answer was nobody". A daemon too old to have the subcommand
//! exits non-zero and never reaches this; one that answered something this
//! module does not understand is a `None`, which the caller turns into a 500. It
//! must never become `Some(None)`, because the control plane reads that as a
//! standing "not activated" and would keep polling a tenant it can never learn
//! anything about — or worse, a future output format would silently read as
//! "nobody ever paired" for the whole fleet.

use chrono::{DateTime, Utc};

/// Read `squelchd token first-paired`'s stdout.
///
/// - `Some(Some(ts))` — a client device first paired at `ts`.
/// - `Some(None)` — the daemon answered, and nobody has ever paired.
/// - `None` — the output is not this command's output. Never half-guessed.
///
/// The FIRST NON-EMPTY LINE decides, and everything after it is ignored. That is
/// the tolerant half of a strict contract: the daemon prints one line and the
/// exec transport may add a trailing newline or a stray blank, neither of which
/// is a disagreement about the answer. A second line carrying a DIFFERENT answer
/// cannot arise from any daemon that ships this command; if one ever did, the
/// first line is the one the daemon put there first.
///
/// The timestamp is normalized to UTC on the way through. The daemon renders `Z`
/// already, so this only matters if a future one ever renders a local offset:
/// the instant is the same either way, and the control plane must not have to
/// know which spelling it got.
pub fn parse_first_paired(stdout: &str) -> Option<Option<DateTime<Utc>>> {
    let line = stdout
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())?;
    if line == "none" {
        return Some(None);
    }
    // Held to RFC3339 exactly, which is what the daemon promises to print. A
    // looser parse would accept half a date and call it an activation.
    let parsed = DateTime::parse_from_rfc3339(line).ok()?;
    Some(Some(parsed.with_timezone(&Utc)))
}

/// The platforms `token activity` may name. A CLOSED set, the daemon's own
/// (`squelch_core::store::ClientPlatform`): a key outside it is a platform a
/// newer daemon learned about and this warden did not, and it is skipped
/// rather than carried onward as a string nobody downstream has a column for.
pub const ACTIVITY_PLATFORMS: [&str; 2] = ["macos", "ios"];

/// What `squelchd token activity` reported, parsed. Every timestamp is an
/// instant in UTC; nothing else is in here, by construction.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct Activity {
    /// The activation fact, the same answer `token first-paired` gives.
    #[serde(serialize_with = "ser_opt_ts")]
    pub first_paired_at: Option<DateTime<Utc>>,
    /// One entry per known platform that has ever connected.
    pub platforms: std::collections::BTreeMap<String, PlatformActivity>,
}

/// One platform's line in [`Activity`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct PlatformActivity {
    #[serde(serialize_with = "ser_ts")]
    pub first_paired_at: DateTime<Utc>,
    #[serde(serialize_with = "ser_opt_ts")]
    pub last_seen_at: Option<DateTime<Utc>>,
    #[serde(serialize_with = "ser_opt_ts")]
    pub last_action_at: Option<DateTime<Utc>>,
}

/// One spelling on the warden's wire, whatever the daemon printed: UTC,
/// seconds, `Z`.
fn ser_ts<S: serde::Serializer>(at: &DateTime<Utc>, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
}

fn ser_opt_ts<S: serde::Serializer>(at: &Option<DateTime<Utc>>, s: S) -> Result<S::Ok, S::Error> {
    match at {
        Some(at) => ser_ts(at, s),
        None => s.serialize_none(),
    }
}

/// Read `squelchd token activity`'s stdout: one line of JSON.
///
/// `None` when the output is not that command's output, which the caller
/// turns into a 500. STRICT where it matters: the line must be a JSON object,
/// every key the contract names must be PRESENT (a missing `last_action_at`
/// is not read as "never acted"), and every timestamp must be RFC3339 or an
/// explicit `null` where the contract allows one. A garbled line must never
/// come out as an empty [`Activity`], because the control plane would store
/// that as "nobody uses this mailbox".
///
/// TOLERANT where a newer daemon could legitimately differ: an extra key is
/// ignored, and a platform outside [`ACTIVITY_PLATFORMS`] is skipped (its
/// value is still held to the shape, so garbage cannot hide under a new key).
/// The first non-empty line decides, as in [`parse_first_paired`].
pub fn parse_activity(stdout: &str) -> Option<Activity> {
    use serde_json::Value;
    let line = stdout
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())?;
    let Value::Object(top) = serde_json::from_str::<Value>(line).ok()? else {
        return None;
    };
    let ts = |v: &Value| -> Option<DateTime<Utc>> {
        Some(
            DateTime::parse_from_rfc3339(v.as_str()?)
                .ok()?
                .with_timezone(&Utc),
        )
    };
    // Present, and either null or a timestamp. `Some(None)` is an explicit
    // null; `None` is a refusal.
    let opt_ts = |obj: &serde_json::Map<String, Value>, key: &str| match obj.get(key)? {
        Value::Null => Some(None),
        v => ts(v).map(Some),
    };

    let first_paired_at = opt_ts(&top, "first_paired_at")?;
    let Value::Object(raw_platforms) = top.get("platforms")? else {
        return None;
    };
    let mut platforms = std::collections::BTreeMap::new();
    for (name, value) in raw_platforms {
        let Value::Object(p) = value else {
            return None;
        };
        let line = PlatformActivity {
            first_paired_at: ts(p.get("first_paired_at")?)?,
            last_seen_at: opt_ts(p, "last_seen_at")?,
            last_action_at: opt_ts(p, "last_action_at")?,
        };
        if ACTIVITY_PLATFORMS.contains(&name.as_str()) {
            platforms.insert(name.clone(), line);
        }
    }
    Some(Activity {
        first_paired_at,
        platforms,
    })
}

/// Whether a failed `token activity` exec failed because the daemon is too
/// old to have the subcommand: clap's own refusal, on stderr.
///
/// READ, NEVER LOGGED. The caller decides on this and logs the reason word,
/// not the stream.
///
/// Its own answer because the control plane must treat that tenant as "not
/// known yet" (the next fleet roll fixes it) rather than as broken or as
/// inactive. Matched on clap's fixed wording naming THIS subcommand, so any
/// other failure (a panic, a store that will not open) stays a 500.
pub fn activity_unsupported(stderr: &str) -> bool {
    stderr.contains("unrecognized subcommand 'activity'")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{
        activity_none_stdout, activity_stdout, activity_unsupported_stderr,
        first_paired_none_stdout, first_paired_stdout,
    };

    /// The fixture is a captured copy of the shipped output. When the daemon's
    /// one line changes, this is the test that says so.
    #[test]
    fn parses_the_daemons_real_first_paired_output() {
        let parsed = parse_first_paired(&first_paired_stdout("2026-03-01T09:30:00Z"))
            .expect("the daemon answered")
            .expect("with a timestamp");
        assert_eq!(parsed.to_rfc3339(), "2026-03-01T09:30:00+00:00");
    }

    /// `none` is an ANSWER, not a failure: this mailbox exists, is running, and
    /// nobody has ever paired a client with it.
    #[test]
    fn none_is_an_answer_and_not_a_refusal() {
        assert_eq!(parse_first_paired(&first_paired_none_stdout()), Some(None));
        // The transport's trailing blank lines say nothing about the answer.
        assert_eq!(parse_first_paired("\n\nnone\n\n"), Some(None));
    }

    /// An offset that is not `Z` is the same instant, and comes back as UTC so
    /// the control plane never sees two spellings of one moment.
    #[test]
    fn a_non_utc_offset_normalizes_rather_than_travelling_onward() {
        let parsed = parse_first_paired("2026-03-01T04:30:00-05:00\n")
            .unwrap()
            .unwrap();
        assert_eq!(parsed.to_rfc3339(), "2026-03-01T09:30:00+00:00");
    }

    /// Anything else is a refusal. `Some(None)` here would be the control plane
    /// told "nobody ever paired" by a daemon that said no such thing.
    #[test]
    fn garbage_is_refused_rather_than_read_as_nobody() {
        for output in [
            "",
            "\n",
            "   \n",
            "None\n",
            "NONE\n",
            "none of your business\n",
            "2026-03-01\n",
            "2026-03-01 09:30:00\n",
            "first paired: 2026-03-01T09:30:00Z\n",
            "error: unrecognized subcommand 'first-paired'\n",
            // A listing, which is the shape the separate subcommand exists to
            // make impossible: it is not one line and it is not a timestamp.
            "   ID  NAME     CREATED\n    1  iPhone   2026-03-01\n",
        ] {
            assert_eq!(parse_first_paired(output), None, "{output:?}");
        }
    }

    /// The fixture is the daemon's real line (the same string
    /// `squelchd`'s own test asserts it prints).
    #[test]
    fn parses_the_daemons_real_activity_output() {
        let parsed = parse_activity(&activity_stdout()).expect("the daemon answered");
        let ts = |s: &str| DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc);
        assert_eq!(parsed.first_paired_at, Some(ts("2026-03-01T09:30:00Z")));
        assert_eq!(
            parsed.platforms["macos"],
            PlatformActivity {
                first_paired_at: ts("2026-03-01T09:30:00Z"),
                last_seen_at: Some(ts("2026-03-09T08:00:00Z")),
                last_action_at: Some(ts("2026-03-08T07:00:00Z")),
            }
        );
        assert_eq!(parsed.platforms["ios"].last_action_at, None);
        assert_eq!(parsed.platforms.len(), 2);

        // Nobody ever paired: an answer, with nothing in it.
        assert_eq!(
            parse_activity(&activity_none_stdout()),
            Some(Activity::default())
        );
        // Re-serialized in the one spelling, nothing added.
        assert_eq!(
            serde_json::to_value(&parsed).unwrap()["platforms"]["ios"],
            serde_json::json!({
                "first_paired_at": "2026-03-02T10:00:00Z",
                "last_seen_at": "2026-03-09T09:00:00Z",
                "last_action_at": null,
            })
        );
    }

    /// A newer daemon may add a key or a platform; neither is a disagreement.
    #[test]
    fn activity_tolerates_what_a_newer_daemon_could_add() {
        let parsed = parse_activity(
            r#"{"first_paired_at":null,"v":2,"platforms":{"android":{"first_paired_at":"2026-03-01T09:30:00Z","last_seen_at":null,"last_action_at":null},"ios":{"first_paired_at":"2026-03-01T04:30:00-05:00","last_seen_at":null,"last_action_at":null,"extra":1}}}"#,
        )
        .unwrap();
        assert_eq!(parsed.platforms.keys().collect::<Vec<_>>(), ["ios"]);
        assert_eq!(
            parsed.platforms["ios"].first_paired_at.to_rfc3339(),
            "2026-03-01T09:30:00+00:00"
        );
    }

    /// Garbled output is a refusal, never an empty answer.
    #[test]
    fn garbled_activity_is_refused_rather_than_read_as_nobody() {
        for output in [
            "",
            "\n",
            "none\n",
            "null\n",
            "[]\n",
            "{}\n",
            r#"{"platforms":{}}"#,
            r#"{"first_paired_at":null}"#,
            r#"{"first_paired_at":"yesterday","platforms":{}}"#,
            r#"{"first_paired_at":null,"platforms":[]}"#,
            r#"{"first_paired_at":null,"platforms":{"ios":null}}"#,
            r#"{"first_paired_at":null,"platforms":{"ios":{"first_paired_at":null,"last_seen_at":null,"last_action_at":null}}}"#,
            r#"{"first_paired_at":null,"platforms":{"ios":{"first_paired_at":"2026-03-01T09:30:00Z","last_action_at":null}}}"#,
            r#"{"first_paired_at":null,"platforms":{"android":{"first_paired_at":"soon"}}}"#,
            "error: unrecognized subcommand 'activity'\n",
        ] {
            assert_eq!(parse_activity(output), None, "{output:?}");
        }
    }

    /// clap's refusal names the subcommand; nothing else counts as "too old".
    #[test]
    fn only_claps_refusal_reads_as_a_daemon_too_old() {
        assert!(activity_unsupported(&activity_unsupported_stderr()));
        assert!(!activity_unsupported(""));
        assert!(!activity_unsupported("thread 'main' panicked at 'store'\n"));
        assert!(!activity_unsupported(
            "error: unrecognized subcommand 'first-paired'\n"
        ));
    }
}
