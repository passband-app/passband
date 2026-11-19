//! The ACCOUNT OWNER's clock: which zone the owner is in, and "now" written in
//! it for an agent to read (issue #526).
//!
//! `crate::timezone` is about correspondents. This is about the one person the
//! mailbox belongs to, and the source is different: the apps send the device's
//! IANA zone on every request ([`ZONE_HEADER`]), the daemon keeps it on the
//! device token's row, and the owner's zone is the one the most recent client
//! connection reported. Travel needs nothing from the user: open the phone in
//! Tokyo and the next agent run reads Tokyo time.
//!
//! Instants stay UTC everywhere they are stored. Only what an agent reads in
//! prose becomes local, through [`OwnerClock`].

use chrono::{DateTime, NaiveDate, SecondsFormat, Utc};
use chrono_tz::Tz;

/// The request header an app names its device zone in, e.g.
/// `X-Passband-Timezone: America/Los_Angeles`. Sent next to
/// [`crate::store::CLIENT_HEADER`] and only ever to the user's own daemon,
/// for the same reason that one is (see there).
pub const ZONE_HEADER: &str = "x-passband-timezone";

/// Longest zone name worth parsing. The longest IANA name is about 32
/// characters ("America/Argentina/ComodRivadavia"); the header is read before
/// the bearer is known to be good, so anything longer is refused by length.
const MAX_ZONE_LEN: usize = 64;

/// A device zone as an app reported it, or `None` for anything that is not an
/// IANA zone `chrono_tz` knows. Unlike [`crate::timezone::place_zone`], `UTC`
/// and `GMT` are accepted: a device set to UTC is in UTC, and that is exactly
/// what the agents should use for it.
pub fn device_zone(raw: &str) -> Option<Tz> {
    let raw = raw.trim();
    if raw.is_empty() || raw.len() > MAX_ZONE_LEN {
        return None;
    }
    raw.parse().ok()
}

/// Where the owner's zone came from, strongest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnerZoneSource {
    /// The newest app connection that reported a zone.
    Client,
    /// The account's primary Google Calendar, when no app has reported one.
    Calendar,
    /// Nothing reported anything.
    Default,
}

impl OwnerZoneSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Client => "client",
            Self::Calendar => "calendar",
            Self::Default => "default",
        }
    }
}

/// The owner's zone and where it came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OwnerZone {
    pub zone: Tz,
    pub source: OwnerZoneSource,
}

impl OwnerZone {
    pub const UTC: Self = Self {
        zone: Tz::UTC,
        source: OwnerZoneSource::Default,
    };
}

impl Default for OwnerZone {
    fn default() -> Self {
        Self::UTC
    }
}

/// One agent run's "now", fixed at the start of the run so a zone that changes
/// mid-run cannot change answers already given.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OwnerClock {
    pub now: DateTime<Utc>,
    pub zone: Tz,
}

impl OwnerClock {
    pub fn new(now: DateTime<Utc>, zone: OwnerZone) -> Self {
        Self {
            now,
            zone: zone.zone,
        }
    }

    /// "now" on the owner's wall clock.
    pub fn local_now(&self) -> DateTime<Tz> {
        self.now.with_timezone(&self.zone)
    }

    /// The owner's calendar date right now.
    pub fn today(&self) -> NaiveDate {
        self.local_now().date_naive()
    }

    /// An instant in the owner's zone as RFC 3339 with its offset, e.g.
    /// `2026-10-19T20:00:00-07:00`. The same instant as the UTC form, so
    /// anything that parses one parses the other.
    pub fn local(&self, at: DateTime<Utc>) -> String {
        at.with_timezone(&self.zone)
            .to_rfc3339_opts(SecondsFormat::Secs, false)
    }

    /// "now" as an agent reads it:
    /// `2026-10-19T16:46:00-07:00 (America/Los_Angeles, Sunday)`.
    pub fn prompt_line(&self) -> String {
        format!(
            "{} ({}, {})",
            self.local(self.now),
            self.zone.name(),
            self.local_now().format("%A")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn device_zone_takes_iana_names_and_refuses_the_rest() {
        assert_eq!(
            device_zone(" America/Los_Angeles "),
            Some(Tz::America__Los_Angeles)
        );
        assert_eq!(device_zone("UTC"), Some(Tz::UTC));
        assert_eq!(device_zone("GMT"), Some(Tz::GMT));
        for bad in [
            "",
            "Mars/Olympus",
            "PDT",
            "America/Los_Angeles\nX",
            &"A/".repeat(40),
        ] {
            assert_eq!(device_zone(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn the_prompt_line_is_local_with_zone_and_weekday() {
        let now = Utc.with_ymd_and_hms(2026, 10, 19, 23, 46, 0).unwrap();
        let la = OwnerZone {
            zone: Tz::America__Los_Angeles,
            source: OwnerZoneSource::Client,
        };
        let clock = OwnerClock::new(now, la);
        assert_eq!(
            clock.prompt_line(),
            "2026-10-19T16:46:00-07:00 (America/Los_Angeles, Monday)"
        );
        assert_eq!(
            clock.today(),
            NaiveDate::from_ymd_opt(2026, 10, 19).unwrap()
        );
    }

    #[test]
    fn an_american_evening_is_still_today_where_utc_is_tomorrow() {
        // 8pm Pacific is 03:00 UTC the next day: the bug in #526.
        let evening = Utc.with_ymd_and_hms(2026, 10, 20, 3, 0, 0).unwrap();
        let clock = OwnerClock::new(
            evening,
            OwnerZone {
                zone: Tz::America__Los_Angeles,
                source: OwnerZoneSource::Client,
            },
        );
        assert_eq!(
            clock.today(),
            NaiveDate::from_ymd_opt(2026, 10, 19).unwrap()
        );
        assert_eq!(clock.local(evening), "2026-10-19T20:00:00-07:00");
        assert_eq!(
            OwnerClock::new(evening, OwnerZone::UTC).today().to_string(),
            "2026-10-20"
        );
    }
}
