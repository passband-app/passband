//! A correspondent's timezone: what ingest captures about it, and the pure
//! inference that turns those captures into a zone on their contact card
//! (docs/CALENDAR.md §7, "How the zone is inferred"). The composer uses it to
//! write a suggested time in both zones; the user sees it, and can correct it,
//! on the person record (`GET /client/person`). HUMAN-DOOR ONLY.
//!
//! Signals, strongest first:
//!
//! 1. A zone the user set by hand. Always wins.
//! 2. The `TZID` of the first `DTSTART` in an invitation that person ORGANIZED
//!    (the iCalendar `ORGANIZER`). The organizer is whatever the `.ics` says,
//!    so the store records an invitation only from AUTHENTICATED mail whose
//!    From is the organizer or the organizer's own organisation (a delegate
//!    or a company calendar address): see `store::sqlite::zones`. The zone is
//!    counted under its canonical name ([`canonical_zone`]). The subject's
//!    "(PDT)" is never read: Google renders it in the RECIPIENT's zone.
//! 3. The UTC offset of the `Date` header on their own inbound, authenticated
//!    mail. Zero is discarded: many clients and nearly every automated sender
//!    stamp UTC, so a zero says nothing about where a person is.
//!
//! An offset is not a zone. Each observation is compared with each curated
//! zone's offset AT THAT OBSERVATION'S INSTANT, so mail from both sides of a
//! daylight-saving change usually pins one zone down, while one season alone
//! matches several (Denver and Phoenix agree all winter) and is served as a
//! best guess.

use chrono::{DateTime, Datelike, Duration, Offset, TimeZone, Utc};
use chrono_tz::Tz;

/// Offsets older than this say little about where someone is now.
pub const OFFSET_WINDOW_DAYS: i64 = 365;
/// Fewer non-zero offsets than this is no inference: two mails are a trip.
pub const MIN_OFFSET_OBSERVATIONS: usize = 3;
/// The share of observations a zone must explain: 9 in 10. Lower lets a
/// month of travel name the wrong home; higher lets one stray stamp (a phone
/// that had not caught up after a flight) veto a clear answer.
pub const ZONE_SHARE_NUMERATOR: usize = 9;
pub const ZONE_SHARE_DENOMINATOR: usize = 10;
/// How many of the newest offset observations vote. Five hundred messages are
/// more than enough to see a 90% majority and both sides of a clock change,
/// and the cap is what keeps a sender with ten thousand messages a year as
/// cheap to recompute as one with five hundred.
pub const MAX_OFFSET_OBSERVATIONS: usize = 500;
/// How many of the newest invitation observations vote.
pub const INVITE_WINDOW: usize = 20;
/// The widest real-world offsets (Baker Island, Line Islands). A Date header
/// outside them is garbage, not an observation.
const MIN_OFFSET_MINUTES: i32 = -12 * 60;
const MAX_OFFSET_MINUTES: i32 = 14 * 60;
/// An invitation part bigger than this is not scanned: real ones are a few
/// KiB, and the scan runs inside ingest.
const ICS_SCAN_CAP_BYTES: usize = 256 * 1024;
/// How many calendar parts of one message are looked at. A real invitation
/// has one (two when the `.ics` is attached as well); a message built with
/// hundreds would otherwise cost that many scans inside ingest.
pub const MAX_CALENDAR_PARTS: usize = 3;
/// The longest organizer address recorded, in bytes: RFC 5321's 64-octet
/// local part, the `@` and a 255-octet domain. Anything longer is not an
/// address, and no observation.
pub const MAX_ORGANIZER_BYTES: usize = 320;
/// Mail dated more than this far ahead of now does not vote. Observations are
/// read NEWEST FIRST up to a cap, so a Date header set in the future would
/// sit at the top of the window and keep its slot for as long as the date
/// stays ahead; a day covers any honest clock that is merely fast.
pub const FUTURE_TOLERANCE_DAYS: i64 = 1;
/// Instants, a fortnight apart, at which a zone's clock is compared with a
/// curated zone's to decide it is that zone under another name. Twenty-six
/// span the year, so both sides of any daylight-saving change are seen.
const CANONICAL_SAMPLES: i64 = 26;

/// The representative zones offsets can be matched to, IN PRIORITY ORDER:
/// where several reach the share, the first one named wins. One zone per
/// distinct rule set (Toronto keeps New York's rules, Paris Berlin's), so a
/// tie is always a real ambiguity rather than two names for one clock.
///
/// The order within an offset is a guess at whom a user's correspondents are
/// likelier to be: New York before Bogota (UTC-5 in winter), Denver before
/// Phoenix (UTC-7 in winter), Los Angeles before Phoenix (UTC-7 in summer),
/// Sydney before Brisbane (UTC+10 in the southern winter), London before
/// Lagos (UTC+1 in the northern summer), Berlin before Lagos and
/// Johannesburg. Most half-hour and 45-minute zones (St. John's, Tehran,
/// Kabul, India, Nepal, Yangon) have no rival at their offsets and so come
/// out high when they match. Darwin and Adelaide are the exception: Adelaide
/// in its winter is Darwin's +9:30, so a winter of Adelaide mail matches both
/// and is served as Darwin at medium, until mail from its summer (+10:30,
/// Adelaide's alone) pins it.
pub const CURATED_ZONES: &[&str] = &[
    // North America
    "America/New_York",
    "America/Chicago",
    "America/Denver",
    "America/Phoenix",
    "America/Los_Angeles",
    "America/Anchorage",
    "Pacific/Honolulu",
    "America/Halifax",
    "America/St_Johns",
    "America/Mexico_City",
    // Central and South America
    "America/Bogota",
    "America/Caracas",
    "America/Santiago",
    "America/Sao_Paulo",
    "America/Argentina/Buenos_Aires",
    // Europe
    "Europe/London",
    "Europe/Berlin",
    "Europe/Athens",
    "Europe/Moscow",
    "Europe/Istanbul",
    // Africa and the Middle East
    "Africa/Lagos",
    "Africa/Johannesburg",
    "Africa/Cairo",
    "Asia/Jerusalem",
    "Africa/Nairobi",
    "Asia/Riyadh",
    "Asia/Tehran",
    "Asia/Dubai",
    // Asia
    "Asia/Kabul",
    "Asia/Karachi",
    "Asia/Kolkata",
    "Asia/Kathmandu",
    "Asia/Dhaka",
    "Asia/Yangon",
    "Asia/Bangkok",
    "Asia/Jakarta",
    "Asia/Shanghai",
    "Asia/Singapore",
    "Asia/Manila",
    "Asia/Tokyo",
    "Asia/Seoul",
    // Oceania
    "Australia/Perth",
    "Australia/Darwin",
    "Australia/Adelaide",
    "Australia/Sydney",
    "Australia/Brisbane",
    "Pacific/Auckland",
];

/// Where an inferred zone came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZoneSource {
    User,
    Invite,
    Offsets,
}

impl ZoneSource {
    pub fn as_str(self) -> &'static str {
        match self {
            ZoneSource::User => "user",
            ZoneSource::Invite => "invite",
            ZoneSource::Offsets => "offsets",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "user" => Some(ZoneSource::User),
            "invite" => Some(ZoneSource::Invite),
            "offsets" => Some(ZoneSource::Offsets),
            _ => None,
        }
    }
}

/// How sure the inference is. `Medium` is a best guess the composer may show
/// but should not act on silently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZoneConfidence {
    High,
    Medium,
}

impl ZoneConfidence {
    pub fn as_str(self) -> &'static str {
        match self {
            ZoneConfidence::High => "high",
            ZoneConfidence::Medium => "medium",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "high" => Some(ZoneConfidence::High),
            "medium" => Some(ZoneConfidence::Medium),
            _ => None,
        }
    }
}

/// A zone, with where it came from and how sure we are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InferredZone {
    /// An IANA name that parses with `chrono-tz`.
    pub zone: String,
    pub source: ZoneSource,
    pub confidence: ZoneConfidence,
}

impl InferredZone {
    /// The zone's UTC offset at `at`, in minutes. `None` only for a name that
    /// no longer parses, which inference and the override's door both refuse.
    pub fn utc_offset_minutes(&self, at: DateTime<Utc>) -> Option<i32> {
        let tz: Tz = self.zone.parse().ok()?;
        Some(offset_minutes_at(tz, at))
    }
}

/// One `Date` header seen on someone's inbound mail: when it was sent, and the
/// offset the sender's clock stamped on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OffsetObservation {
    pub at: DateTime<Utc>,
    pub offset_minutes: i32,
}

/// What one invitation says: who organized it, and the zone its start is in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InviteZone {
    /// The `ORGANIZER`'s mailto address, lowercased.
    pub organizer: String,
    /// A valid IANA name.
    pub zone: String,
}

fn offset_minutes_at(tz: Tz, at: DateTime<Utc>) -> i32 {
    tz.offset_from_utc_datetime(&at.naive_utc())
        .fix()
        .local_minus_utc()
        / 60
}

/// An IANA zone a person can be in, or `None`. Validation only: the name is
/// returned as given (see [`canonical_zone`] for counting).
///
/// A place has a `/` ("America/New_York", "US/Pacific"). Every name without
/// one parses but names no place: the fixed offsets ("EST", "MST", "HST",
/// wrong by an hour all summer wherever they are read), the POSIX-style rule
/// sets ("EST5EDT", "CET") and the UTC aliases ("UTC", "GMT", "Zulu"; an
/// invitation in UTC is a UTC time, which capture treats like a `Z` start).
/// `Etc/*` has a `/` and is the same thing again ("Etc/GMT+5").
pub fn place_zone(name: &str) -> Option<Tz> {
    let name = name.trim();
    if !name.contains('/') || name.starts_with("Etc/") {
        return None;
    }
    name.parse().ok()
}

/// A zone under the name inference COUNTS it by, or `None` when it is no
/// place ([`place_zone`]).
///
/// A curated zone is itself. Any other place whose clock agrees with a
/// curated zone's at [`CANONICAL_SAMPLES`] instants a fortnight apart across
/// `at`'s calendar year is that curated zone for every purpose inference
/// has: "US/Pacific" counts as America/Los_Angeles, "Asia/Calcutta" as
/// Asia/Kolkata, and (by the curated list's one-zone-per-rule-set design)
/// "Europe/Paris" as Europe/Berlin. Without this, two invitations from one
/// person split their vote across two names for one clock. Where several
/// curated zones agree at every sample (Athens and Jerusalem change their
/// clocks two days apart, which a fortnight can step over), the first that
/// also agrees on every day of the year wins, else the first in priority
/// order. Curated zones that keep the same clock all year cannot be told
/// apart this way, so an alias of the later one counts as the earlier:
/// "America/Buenos_Aires" as America/Sao_Paulo (both UTC-3 since Brazil
/// dropped daylight saving), "Asia/Hong_Kong" as Asia/Shanghai. The offsets
/// inference names the same zone for the same mail. A place that agrees with
/// no curated zone keeps its own name.
///
/// `at` is the observation's instant: zones change their rules over the
/// years, and the year an invitation was sent is the one it speaks for.
pub fn canonical_zone(name: &str, at: DateTime<Utc>) -> Option<String> {
    let tz = place_zone(name)?;
    if CURATED_ZONES.contains(&tz.name()) {
        return Some(tz.name().to_string());
    }
    let year_start = Utc.with_ymd_and_hms(at.year(), 1, 1, 12, 0, 0).single()?;
    let same_clock = |other: Tz, step_days: i64, samples: i64| {
        (0..samples).all(|k| {
            let t = year_start + Duration::days(step_days * k);
            offset_minutes_at(tz, t) == offset_minutes_at(other, t)
        })
    };
    let agreeing: Vec<Tz> = CURATED_ZONES
        .iter()
        .filter_map(|name| name.parse::<Tz>().ok())
        .filter(|curated| same_clock(*curated, 14, CANONICAL_SAMPLES))
        .collect();
    let chosen = match agreeing.as_slice() {
        [] => tz,
        [only] => *only,
        several => several
            .iter()
            .copied()
            .find(|curated| same_clock(*curated, 1, 365))
            .unwrap_or(several[0]),
    };
    Some(chosen.name().to_string())
}

/// Decide a person's zone from everything known about them.
///
/// `invites` is the invitation zones they organized, NEWEST FIRST; only the
/// newest [`INVITE_WINDOW`] vote, each under its [`canonical_zone`] name (at
/// `now`'s year: the store already wrote them canonical at their own, and a
/// curated name is itself in any year), and a name that is no place does not
/// vote. `offsets` is the authenticated, non-sent, non-spam mail they wrote,
/// in any order; zero offsets and anything older than [`OFFSET_WINDOW_DAYS`]
/// before `now` are dropped here, and only the newest
/// [`MAX_OFFSET_OBSERVATIONS`] vote, whatever the caller passed, so the rule
/// lives in one place. Mail dated in the future is the store's to drop
/// ([`FUTURE_TOLERANCE_DAYS`]): it matters for which rows fill the cap, and
/// the cap is applied in the read.
pub fn infer_zone(
    user_override: Option<&str>,
    invites: &[String],
    offsets: &[OffsetObservation],
    now: DateTime<Utc>,
) -> Option<InferredZone> {
    if let Some(tz) = user_override.and_then(|z| z.trim().parse::<Tz>().ok()) {
        return Some(InferredZone {
            zone: tz.name().to_string(),
            source: ZoneSource::User,
            confidence: ZoneConfidence::High,
        });
    }
    if let Some(zone) = invite_majority(invites, now) {
        return Some(InferredZone {
            zone,
            source: ZoneSource::Invite,
            confidence: ZoneConfidence::High,
        });
    }
    offsets_zone(offsets, now)
}

/// The most frequent zone among the newest invitations, when it holds at
/// least half of them. A tie between two zones goes to the one seen most
/// recently, which is the order `invites` arrives in.
fn invite_majority(invites: &[String], now: DateTime<Utc>) -> Option<String> {
    let window: Vec<String> = invites
        .iter()
        .take(INVITE_WINDOW)
        .filter_map(|z| canonical_zone(z, now))
        .collect();
    if window.is_empty() {
        return None;
    }
    let mut best: Option<(&str, usize)> = None;
    for zone in &window {
        let n = window.iter().filter(|z| *z == zone).count();
        if best.is_none_or(|(_, m)| n > m) {
            best = Some((zone, n));
        }
    }
    let (zone, n) = best?;
    (n * 2 >= window.len()).then(|| zone.to_string())
}

fn offsets_zone(offsets: &[OffsetObservation], now: DateTime<Utc>) -> Option<InferredZone> {
    let since = now - Duration::days(OFFSET_WINDOW_DAYS);
    let mut seen: Vec<OffsetObservation> = offsets
        .iter()
        .copied()
        .filter(|o| o.offset_minutes != 0 && o.at >= since)
        .collect();
    seen.sort_by(|a, b| b.at.cmp(&a.at));
    seen.truncate(MAX_OFFSET_OBSERVATIONS);
    if seen.len() < MIN_OFFSET_OBSERVATIONS {
        return None;
    }
    // Every curated zone that explains enough of the mail, in priority order.
    // Integer arithmetic for the share, so 27 of 30 is exactly 0.9.
    let reaching: Vec<&str> = CURATED_ZONES
        .iter()
        .copied()
        .filter(|name| {
            let Ok(tz) = name.parse::<Tz>() else {
                return false;
            };
            let matched = seen
                .iter()
                .filter(|o| offset_minutes_at(tz, o.at) == o.offset_minutes)
                .count();
            matched * ZONE_SHARE_DENOMINATOR >= seen.len() * ZONE_SHARE_NUMERATOR
        })
        .collect();
    let zone = *reaching.first()?;
    // HIGH ONLY WHEN NO OTHER LISTED ZONE EXPLAINS THE SAME MAIL. Mail from
    // both sides of a daylight-saving change is how that usually happens (it
    // rules out every zone without the same change on the same dates), but it
    // is not sufficient on its own: Athens, Jerusalem and Cairo are all
    // UTC+2 in winter and UTC+3 in summer, so a year of mail from any of them
    // matches all three, and the guess is only as sure as it is unique.
    let confidence = if reaching.len() == 1 {
        ZoneConfidence::High
    } else {
        ZoneConfidence::Medium
    };
    Some(InferredZone {
        zone: zone.to_string(),
        source: ZoneSource::Offsets,
        confidence,
    })
}

// ---- capture --------------------------------------------------------------

/// The `Date` header's offset from UTC in minutes, east positive. `None` for
/// an offset no clock on Earth uses. Zero is returned as zero: the message
/// row records what the header said, and inference is what ignores it.
pub fn date_offset_minutes(date: &mail_parser::DateTime) -> Option<i32> {
    let magnitude = i32::from(date.tz_hour) * 60 + i32::from(date.tz_minute);
    let offset = if date.tz_before_gmt {
        -magnitude
    } else {
        magnitude
    };
    (MIN_OFFSET_MINUTES..=MAX_OFFSET_MINUTES)
        .contains(&offset)
        .then_some(offset)
}

/// The first invitation in a parsed message that yields an observation:
/// a `text/calendar` (or `application/ics`) part, or any part named `*.ics`.
/// Only the first [`MAX_CALENDAR_PARTS`] such parts are looked at, an
/// oversized one included. Nested messages are not descended into: a
/// forwarded invitation is the forwarder's mail, and its parts belong to the
/// inner message.
pub fn invite_zone(message: &mail_parser::Message) -> Option<InviteZone> {
    use mail_parser::MimeHeaders;
    message
        .parts
        .iter()
        .filter(|part| {
            let typed = part.content_type().is_some_and(|ct| {
                let sub = ct.subtype().unwrap_or_default();
                (ct.ctype().eq_ignore_ascii_case("text") && sub.eq_ignore_ascii_case("calendar"))
                    || (ct.ctype().eq_ignore_ascii_case("application")
                        && sub.eq_ignore_ascii_case("ics"))
            });
            typed
                || part
                    .attachment_name()
                    .is_some_and(|n| n.trim().to_ascii_lowercase().ends_with(".ics"))
        })
        .take(MAX_CALENDAR_PARTS)
        .find_map(|part| {
            let bytes = part.contents();
            if bytes.len() > ICS_SCAN_CAP_BYTES {
                return None;
            }
            scan_ics(&String::from_utf8_lossy(bytes))
        })
}

/// Read the two things inference needs out of an iCalendar body: the first
/// VEVENT's `ORGANIZER` mailto address and the `TZID` of its first `DTSTART`.
/// A line-oriented scan, not a parser: folded lines (a line starting with a
/// space or tab continues the previous one) are unfolded first, and quoted
/// parameter values may hold `:` and `;`.
///
/// No observation (None) for: no VEVENT, no organizer address, a `DTSTART`
/// in UTC (`...Z`), a floating or all-day start (no TZID), or a TZID that is
/// not an IANA name a person can be in ("Eastern Standard Time", "UTC").
pub fn scan_ics(ics: &str) -> Option<InviteZone> {
    let mut lines: Vec<String> = Vec::new();
    for raw in ics.split('\n') {
        let raw = raw.strip_suffix('\r').unwrap_or(raw);
        match raw.strip_prefix([' ', '\t']) {
            Some(rest) if !lines.is_empty() => lines.last_mut()?.push_str(rest),
            _ => lines.push(raw.to_string()),
        }
    }

    let mut in_event = false;
    let mut organizer: Option<String> = None;
    let mut start_zone: Option<Option<String>> = None;
    for line in &lines {
        let Some((name, params, value)) = split_property(line) else {
            continue;
        };
        if !in_event {
            if name.eq_ignore_ascii_case("BEGIN") && value.trim().eq_ignore_ascii_case("VEVENT") {
                in_event = true;
            }
            continue;
        }
        if name.eq_ignore_ascii_case("END") && value.trim().eq_ignore_ascii_case("VEVENT") {
            break;
        }
        if name.eq_ignore_ascii_case("ORGANIZER") && organizer.is_none() {
            let value = value.trim();
            let addr = value
                .get(..7)
                .filter(|p| p.eq_ignore_ascii_case("mailto:"))
                .map(|_| value[7..].trim().to_ascii_lowercase())
                .filter(|a| a.contains('@') && !a.contains(char::is_whitespace));
            organizer = addr;
        } else if name.eq_ignore_ascii_case("DTSTART") && start_zone.is_none() {
            let value = value.trim();
            let timed = value.contains(['T', 't']);
            let utc = value.ends_with(['Z', 'z']);
            let tzid = params
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("TZID"))
                .map(|(_, v)| v.trim().to_string());
            start_zone = Some(match tzid {
                Some(z) if timed && !utc => place_zone(&z).map(|tz| tz.name().to_string()),
                _ => None,
            });
        }
    }
    Some(InviteZone {
        organizer: organizer?,
        zone: start_zone.flatten()?,
    })
}

/// An iCalendar content line split up: name, `(param, value)` pairs, value.
type Property<'a> = (&'a str, Vec<(String, String)>, &'a str);

/// `NAME;P1=V1;P2="V:2":value` into its name, parameters and value, honouring
/// double quotes in parameter values. `None` for a line with no `:`.
fn split_property(line: &str) -> Option<Property<'_>> {
    let mut quoted = false;
    let mut colon = None;
    for (i, c) in line.char_indices() {
        match c {
            '"' => quoted = !quoted,
            ':' if !quoted => {
                colon = Some(i);
                break;
            }
            _ => {}
        }
    }
    let colon = colon?;
    let (head, value) = (&line[..colon], &line[colon + 1..]);
    let mut parts: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for c in head.chars() {
        match c {
            '"' => quoted = !quoted,
            ';' if !quoted => parts.push(std::mem::take(&mut current)),
            _ => current.push(c),
        }
    }
    parts.push(current);
    let name_len = head.find(';').unwrap_or(head.len());
    let params = parts
        .into_iter()
        .skip(1)
        .filter_map(|p| {
            let (k, v) = p.split_once('=')?;
            Some((k.trim().to_string(), v.to_string()))
        })
        .collect();
    Some((head[..name_len].trim(), params, value))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc(y: i32, m: u32, d: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, 15, 0, 0).unwrap()
    }

    fn now() -> DateTime<Utc> {
        utc(2026, 10, 1)
    }

    fn obs(at: DateTime<Utc>, offset_minutes: i32) -> OffsetObservation {
        OffsetObservation { at, offset_minutes }
    }

    /// `n` observations a week apart ending at `last`, each stamped with
    /// `zone`'s real offset at its instant.
    fn stamped(zone: &str, last: DateTime<Utc>, n: i64) -> Vec<OffsetObservation> {
        let tz: Tz = zone.parse().unwrap();
        (0..n)
            .map(|i| {
                let at = last - Duration::days(7 * i);
                obs(at, offset_minutes_at(tz, at))
            })
            .collect()
    }

    fn zone_of(z: Option<InferredZone>) -> Option<(String, ZoneSource, ZoneConfidence)> {
        z.map(|z| (z.zone, z.source, z.confidence))
    }

    #[test]
    fn every_curated_zone_parses_and_is_a_place() {
        for name in CURATED_ZONES {
            assert!(place_zone(name).is_some(), "{name}");
        }
        assert!((40..=50).contains(&CURATED_ZONES.len()));
    }

    #[test]
    fn a_daylight_saving_change_seen_on_both_sides_pins_new_york_high() {
        // January (-300) and July (-240) mail. Bogota matches only the winter
        // half, so the only zone at 0.9 explains both offsets.
        let mut seen = vec![
            obs(utc(2026, 1, 10), -300),
            obs(utc(2026, 1, 20), -300),
            obs(utc(2026, 2, 3), -300),
        ];
        seen.extend([
            obs(utc(2026, 7, 1), -240),
            obs(utc(2026, 7, 9), -240),
            obs(utc(2026, 8, 2), -240),
        ]);
        assert_eq!(
            zone_of(infer_zone(None, &[], &seen, now())),
            Some((
                "America/New_York".into(),
                ZoneSource::Offsets,
                ZoneConfidence::High
            ))
        );
    }

    #[test]
    fn one_season_is_medium_when_another_listed_zone_ties() {
        // Denver in winter is UTC-7, which is Phoenix all year.
        let winter = [
            obs(utc(2026, 1, 5), -420),
            obs(utc(2026, 1, 12), -420),
            obs(utc(2026, 2, 2), -420),
        ];
        assert_eq!(
            zone_of(infer_zone(None, &[], &winter, now())),
            Some((
                "America/Denver".into(),
                ZoneSource::Offsets,
                ZoneConfidence::Medium
            ))
        );
        // Sydney in its winter is UTC+10, which is Brisbane all year.
        let southern_winter = [
            obs(utc(2026, 6, 5), 600),
            obs(utc(2026, 6, 12), 600),
            obs(utc(2026, 7, 2), 600),
        ];
        assert_eq!(
            zone_of(infer_zone(None, &[], &southern_winter, now())),
            Some((
                "Australia/Sydney".into(),
                ZoneSource::Offsets,
                ZoneConfidence::Medium
            ))
        );
    }

    #[test]
    fn a_change_seen_is_still_medium_when_another_zone_shares_it() {
        // Athens and Jerusalem are both +2 in winter and +3 in summer; their
        // changes fall on different days, which this mail does not straddle.
        let seen = [
            obs(utc(2026, 1, 10), 120),
            obs(utc(2026, 1, 20), 120),
            obs(utc(2026, 7, 1), 180),
            obs(utc(2026, 7, 9), 180),
        ];
        assert_eq!(
            zone_of(infer_zone(None, &[], &seen, now())),
            Some((
                "Europe/Athens".into(),
                ZoneSource::Offsets,
                ZoneConfidence::Medium
            ))
        );
    }

    #[test]
    fn one_season_is_high_when_no_other_listed_zone_ties() {
        // India never changes its clocks and nobody else is at +5:30; Nepal's
        // +5:45 likewise. Neither is confused with the other.
        let india = stamped("Asia/Kolkata", utc(2026, 9, 1), 5);
        assert!(india.iter().all(|o| o.offset_minutes == 330));
        assert_eq!(
            zone_of(infer_zone(None, &[], &india, now())),
            Some((
                "Asia/Kolkata".into(),
                ZoneSource::Offsets,
                ZoneConfidence::High
            ))
        );
        let nepal = stamped("Asia/Kathmandu", utc(2026, 9, 1), 5);
        assert_eq!(
            infer_zone(None, &[], &nepal, now()).unwrap().zone,
            "Asia/Kathmandu"
        );
        // Adelaide's +10:30 summer is its own; Darwin never sees it.
        let adelaide = stamped("Australia/Adelaide", utc(2026, 2, 20), 10);
        assert!(adelaide.iter().all(|o| o.offset_minutes == 630));
        assert_eq!(
            zone_of(infer_zone(None, &[], &adelaide, now())),
            Some((
                "Australia/Adelaide".into(),
                ZoneSource::Offsets,
                ZoneConfidence::High
            ))
        );
    }

    #[test]
    fn the_offset_is_compared_at_the_observations_own_instant() {
        // A New York sender's summer mail (-240), inferred in December, when
        // New York is -300. Comparing with the zone's offset "now" would
        // reject every one of these.
        let summer = stamped("America/New_York", utc(2026, 8, 20), 6);
        assert!(summer.iter().all(|o| o.offset_minutes == -240));
        let december = utc(2026, 12, 15);
        let z = infer_zone(None, &[], &summer, december).unwrap();
        assert_eq!(z.zone, "America/New_York");
        // One season: Caracas is -240 all year, and Santiago is in its
        // southern winter at these instants, so the guess is medium.
        assert_eq!(z.confidence, ZoneConfidence::Medium);
    }

    #[test]
    fn fewer_than_three_observations_is_none() {
        let two = [obs(utc(2026, 9, 1), -300), obs(utc(2026, 9, 2), -300)];
        assert_eq!(infer_zone(None, &[], &two, now()), None);
    }

    #[test]
    fn mixed_offsets_are_none() {
        // A traveller, or a shared mailbox: no zone explains 90%.
        let seen = [
            obs(utc(2026, 9, 1), -240),
            obs(utc(2026, 9, 2), -240),
            obs(utc(2026, 9, 3), 120),
            obs(utc(2026, 9, 4), 120),
            obs(utc(2026, 9, 5), 540),
        ];
        assert_eq!(infer_zone(None, &[], &seen, now()), None);
        // Eight in ten is a majority but not a home.
        let mut eighty = stamped("America/Chicago", utc(2026, 9, 20), 8);
        eighty.extend([obs(utc(2026, 9, 21), 60), obs(utc(2026, 9, 22), 60)]);
        assert_eq!(infer_zone(None, &[], &eighty, now()), None);
        // One stray in ten is tolerated.
        let mut mostly = stamped("America/Chicago", utc(2026, 9, 20), 9);
        mostly.push(obs(utc(2026, 9, 21), 60));
        assert_eq!(
            infer_zone(None, &[], &mostly, now()).unwrap().zone,
            "America/Chicago"
        );
    }

    #[test]
    fn a_utc_only_sender_is_none_not_utc() {
        // January, when London is UTC+0: were zeros observations, every one
        // would match London and name a zone for a mail server.
        let seen: Vec<_> = (1..=30).map(|d| obs(utc(2026, 1, d), 0)).collect();
        assert_eq!(infer_zone(None, &[], &seen, now()), None);
        // Zeros do not dilute a real signal either: they are not observations.
        let mut mixed = seen.clone();
        mixed.extend(stamped("Asia/Tokyo", utc(2026, 9, 25), 3));
        assert_eq!(
            infer_zone(None, &[], &mixed, now()).unwrap().zone,
            "Asia/Tokyo"
        );
    }

    #[test]
    fn only_the_newest_observations_vote() {
        // Sixty days of Tokyo mail, then the newest 500 from New York. Past
        // the cap, the Tokyo mail does not vote; counted, its 60 of 560 would
        // sink New York's share to 0.89 and leave no zone at all.
        let ny: Tz = "America/New_York".parse().unwrap();
        let mut seen: Vec<OffsetObservation> = (0..MAX_OFFSET_OBSERVATIONS as i64)
            .map(|i| {
                let at = utc(2026, 9, 30) - Duration::minutes(10 * i);
                obs(at, offset_minutes_at(ny, at))
            })
            .collect();
        seen.extend((0..60).map(|d| obs(utc(2026, 8, 1) - Duration::days(d), 540)));
        assert_eq!(
            infer_zone(None, &[], &seen, now()).unwrap().zone,
            "America/New_York"
        );
    }

    #[test]
    fn offsets_older_than_a_year_do_not_count() {
        let old = stamped("Asia/Tokyo", utc(2025, 6, 1), 10);
        assert_eq!(infer_zone(None, &[], &old, now()), None);
    }

    #[test]
    fn an_invitation_majority_beats_offsets_and_needs_half() {
        let offsets = stamped("America/New_York", utc(2026, 9, 20), 10);
        let invites = vec![
            "Europe/Berlin".to_string(),
            "Europe/Berlin".to_string(),
            "America/New_York".to_string(),
        ];
        assert_eq!(
            zone_of(infer_zone(None, &invites, &offsets, now())),
            Some((
                "Europe/Berlin".into(),
                ZoneSource::Invite,
                ZoneConfidence::High
            ))
        );
        // Three zones, none with half: the invitations do not decide, and the
        // offsets do.
        let split = vec![
            "Europe/Berlin".to_string(),
            "Asia/Tokyo".to_string(),
            "America/Chicago".to_string(),
        ];
        assert_eq!(
            infer_zone(None, &split, &offsets, now()).unwrap().source,
            ZoneSource::Offsets
        );
        // Only the newest twenty vote.
        let mut stale = vec!["Asia/Tokyo".to_string(); 20];
        stale.extend(vec!["Europe/Berlin".to_string(); 30]);
        assert_eq!(
            infer_zone(None, &stale, &[], now()).unwrap().zone,
            "Asia/Tokyo"
        );
    }

    #[test]
    fn the_override_beats_everything_and_clearing_it_restores_inference() {
        let offsets = stamped("America/New_York", utc(2026, 9, 20), 10);
        let invites = vec!["Europe/Berlin".to_string()];
        assert_eq!(
            zone_of(infer_zone(
                Some("America/Chicago"),
                &invites,
                &offsets,
                now()
            )),
            Some((
                "America/Chicago".into(),
                ZoneSource::User,
                ZoneConfidence::High
            ))
        );
        assert_eq!(
            infer_zone(None, &invites, &offsets, now()).unwrap().source,
            ZoneSource::Invite
        );
        // An override that no longer parses is no override.
        assert_eq!(
            infer_zone(Some("Mars/Olympus_Mons"), &invites, &offsets, now())
                .unwrap()
                .source,
            ZoneSource::Invite
        );
    }

    #[test]
    fn date_offsets_carry_sign_and_half_hours() {
        let date = |before: bool, h: u8, m: u8| mail_parser::DateTime {
            year: 2026,
            month: 9,
            day: 1,
            hour: 9,
            minute: 0,
            second: 0,
            tz_before_gmt: before,
            tz_hour: h,
            tz_minute: m,
        };
        assert_eq!(date_offset_minutes(&date(true, 4, 0)), Some(-240));
        assert_eq!(date_offset_minutes(&date(false, 5, 30)), Some(330));
        assert_eq!(date_offset_minutes(&date(false, 5, 45)), Some(345));
        assert_eq!(date_offset_minutes(&date(true, 3, 30)), Some(-210));
        assert_eq!(date_offset_minutes(&date(false, 0, 0)), Some(0));
        assert_eq!(date_offset_minutes(&date(false, 15, 0)), None);
        assert_eq!(date_offset_minutes(&date(true, 13, 0)), None);
    }

    const INVITE: &str = "BEGIN:VCALENDAR\r\n\
        METHOD:REQUEST\r\n\
        BEGIN:VTIMEZONE\r\n\
        TZID:America/Los_Angeles\r\n\
        BEGIN:STANDARD\r\n\
        DTSTART:19701101T020000\r\n\
        END:STANDARD\r\n\
        END:VTIMEZONE\r\n\
        BEGIN:VEVENT\r\n\
        DTSTART;TZID=America/Los_Angeles:20261006T140000\r\n\
        DTEND;TZID=America/Los_Angeles:20261006T150000\r\n\
        ORGANIZER;CN=\"Sam: the organizer\";SENT-BY=\"mailto:cal@x.com\":mailto:Sam@\r\n \
         Example.com\r\n\
        SUMMARY:Sync\r\n\
        END:VEVENT\r\n\
        END:VCALENDAR\r\n";

    #[test]
    fn an_invitation_yields_its_organizer_and_start_zone() {
        assert_eq!(
            scan_ics(INVITE),
            Some(InviteZone {
                organizer: "sam@example.com".into(),
                zone: "America/Los_Angeles".into(),
            })
        );
        // A quoted TZID, and LF line endings.
        let quoted = "BEGIN:VEVENT\nORGANIZER:MAILTO:a@b.co\nDTSTART;TZID=\"Europe/Berlin\":20261006T140000\nEND:VEVENT\n";
        assert_eq!(scan_ics(quoted).unwrap().zone, "Europe/Berlin");
    }

    #[test]
    fn windows_utc_floating_and_date_starts_record_nothing() {
        let with = |dtstart: &str| {
            format!("BEGIN:VEVENT\r\nORGANIZER:mailto:a@b.co\r\n{dtstart}\r\nEND:VEVENT\r\n")
        };
        for dtstart in [
            "DTSTART;TZID=Eastern Standard Time:20261006T140000",
            "DTSTART:20261006T140000Z",
            "DTSTART;TZID=America/New_York:20261006T140000Z",
            "DTSTART:20261006T140000",
            "DTSTART;VALUE=DATE:20261006",
            "DTSTART;TZID=UTC:20261006T140000",
            "DTSTART;TZID=Etc/GMT+5:20261006T140000",
            "DTSTART;TZID=EST:20260706T140000",
            "DTSTART;TZID=MST:20261006T140000",
            "DTSTART;TZID=HST:20261006T140000",
            "DTSTART;TZID=EST5EDT:20261006T140000",
        ] {
            assert_eq!(scan_ics(&with(dtstart)), None, "{dtstart}");
        }
        // No organizer, or an organizer that is not an address.
        let no_org = "BEGIN:VEVENT\r\nDTSTART;TZID=Asia/Tokyo:20261006T140000\r\nEND:VEVENT\r\n";
        assert_eq!(scan_ics(no_org), None);
        // Only the first VEVENT's first DTSTART counts.
        let two = "BEGIN:VEVENT\r\nORGANIZER:mailto:a@b.co\r\nDTSTART:20261006T140000Z\r\n\
                   END:VEVENT\r\nBEGIN:VEVENT\r\nORGANIZER:mailto:a@b.co\r\n\
                   DTSTART;TZID=Asia/Tokyo:20261006T140000\r\nEND:VEVENT\r\n";
        assert_eq!(scan_ics(two), None);
        assert_eq!(scan_ics("garbage\0\u{fffd}"), None);
        assert_eq!(scan_ics(""), None);
    }

    #[test]
    fn a_fixed_offset_abbreviation_is_no_place() {
        // "EST" is a valid tz name for a clock fixed at -5, which is New York
        // only in winter. Without a `/` nothing is a place.
        for name in [
            "EST",
            "MST",
            "HST",
            "EST5EDT",
            "CET",
            "UTC",
            "GMT",
            "Etc/GMT+5",
        ] {
            assert!(name.parse::<Tz>().is_ok(), "{name} parses");
            assert_eq!(place_zone(name), None, "{name}");
            assert_eq!(canonical_zone(name, now()), None, "{name}");
        }
        // Validation keeps the name as given: only counting canonicalises.
        assert_eq!(place_zone(" US/Pacific ").unwrap().name(), "US/Pacific");
    }

    #[test]
    fn an_alias_counts_as_the_curated_zone_with_its_clock() {
        for (alias, curated) in [
            ("US/Pacific", "America/Los_Angeles"),
            ("US/Eastern", "America/New_York"),
            ("America/Toronto", "America/New_York"),
            ("Asia/Calcutta", "Asia/Kolkata"),
            ("Asia/Katmandu", "Asia/Kathmandu"),
            // Same clock all year as Sao Paulo, which is listed first: the
            // name is lost, the clock is not (see `canonical_zone`).
            ("America/Buenos_Aires", "America/Sao_Paulo"),
            (
                "America/Argentina/Buenos_Aires",
                "America/Argentina/Buenos_Aires",
            ),
            ("Europe/Paris", "Europe/Berlin"),
            // Athens and Jerusalem agree at all 26 fortnightly samples of
            // 2026 and differ for two days in March: the daily check decides.
            ("Asia/Tel_Aviv", "Asia/Jerusalem"),
            // A curated zone is itself, whatever else shares its clock.
            ("Asia/Jerusalem", "Asia/Jerusalem"),
            ("Australia/Brisbane", "Australia/Brisbane"),
        ] {
            assert_eq!(
                canonical_zone(alias, now()).as_deref(),
                Some(curated),
                "{alias}"
            );
        }
        // A place no curated zone keeps time with stays itself.
        assert_eq!(
            canonical_zone("Pacific/Chatham", now()).as_deref(),
            Some("Pacific/Chatham")
        );
    }

    #[test]
    fn us_pacific_and_los_angeles_vote_together() {
        let invites = vec![
            "US/Pacific".to_string(),
            "America/Los_Angeles".to_string(),
            "America/New_York".to_string(),
        ];
        assert_eq!(
            zone_of(infer_zone(None, &invites, &[], now())),
            Some((
                "America/Los_Angeles".into(),
                ZoneSource::Invite,
                ZoneConfidence::High
            ))
        );
        // "EST" is no vote at all, not a vote for New York: one Tokyo
        // invitation is then a majority of one.
        let est = vec![
            "EST".to_string(),
            "EST".to_string(),
            "Asia/Tokyo".to_string(),
        ];
        assert_eq!(
            infer_zone(None, &est, &[], now()).unwrap().zone,
            "Asia/Tokyo"
        );
    }

    /// A message with `parts` calendar parts, in order, as a Google-style
    /// `multipart/mixed`.
    fn with_calendar_parts(parts: &[&str]) -> Vec<u8> {
        let mut eml = String::from(
            "From: sam@example.com\r\nTo: me@example.com\r\nSubject: x\r\n\
             MIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=\"b1\"\r\n\r\n",
        );
        for ics in parts {
            eml.push_str("--b1\r\nContent-Type: text/calendar; charset=UTF-8\r\n\r\n");
            eml.push_str(ics);
            eml.push_str("\r\n");
        }
        eml.push_str("--b1--\r\n");
        eml.into_bytes()
    }

    #[test]
    fn the_fourth_calendar_part_is_not_scanned() {
        let junk = "BEGIN:VCALENDAR\r\nEND:VCALENDAR";
        let good = "BEGIN:VEVENT\r\nORGANIZER:mailto:sam@example.com\r\n\
                    DTSTART;TZID=Asia/Tokyo:20261006T140000\r\nEND:VEVENT";
        let parse = |bytes: &[u8]| {
            let message = mail_parser::MessageParser::default()
                .parse(bytes)
                .expect("parses");
            invite_zone(&message)
        };
        // The third part is still read...
        let third = with_calendar_parts(&[junk, junk, good]);
        assert_eq!(parse(&third).unwrap().zone, "Asia/Tokyo");
        // ...the fourth is not.
        let fourth = with_calendar_parts(&[junk, junk, junk, good]);
        assert_eq!(parse(&fourth), None);
    }
}
