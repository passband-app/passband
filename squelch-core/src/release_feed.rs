//! TELLING PEOPLE A NEW VERSION IS OUT, even when the app is not running.
//!
//! The Mac app announces its own updates (Sparkle, then a local banner), but
//! only while it runs. A Mac where Passband was quit, or a person who drifted
//! off to the phone or away entirely, hears nothing. So each daemon reads the
//! PUBLIC release feed (the Mac's Sparkle appcast, which is published the
//! moment a release is downloadable) and, once per version, pushes a release
//! banner to the devices it already holds tokens for. The relay stays blind:
//! it has no device registry, so the daemon that owns the tokens is the only
//! place a fan-out like this can start.
//!
//! WHO HEARS IT ([`decide`]):
//! - Nobody, while the release is younger than [`GRACE`]: a running Mac picks
//!   the update up on its own within the hour, and its own banner says so.
//! - Nobody, ever, once any of the account's Macs has connected running the
//!   new version (or newer). They have it.
//! - Every registered Mac otherwise. A Mac that is running swallows the push
//!   (its Sparkle banner covers it); a Mac where Passband is quit shows it.
//! - The phones too, but only for an account that has had a Mac and has not
//!   connected from one for [`MAC_ACTIVE`]. Somebody using the Mac every day
//!   does not need their phone to buzz about it; somebody who stopped is who
//!   this is for. A phone-only account is not told about a Mac release.
//!
//! ONCE PER VERSION: the newest version announced (or found already
//! installed) is kept in `app_settings` under [`ANNOUNCED_KEY`], so a restart
//! does not repeat it and a release that was skipped is never sent later.

use chrono::{DateTime, Duration, Utc};
use regex::Regex;
use std::sync::LazyLock;

use crate::store::{ClientPlatform, ClientSeen};

/// The Mac's Sparkle feed. Public, and the same bytes every Mac fetches hourly.
pub const FEED_URL: &str = "https://passband.app/appcast.xml";

/// `app_settings` key holding the newest Mac version this account was told
/// about (or was found already running).
pub const ANNOUNCED_KEY: &str = "push.release_announced.macos";

/// How long a release waits before anybody is pushed about it. Sparkle checks
/// hourly; this leaves running Macs time to update and say so first.
pub const GRACE: Duration = Duration::hours(6);

/// A Mac seen this recently counts as in use, and its owner's phone is left
/// alone.
pub const MAC_ACTIVE: Duration = Duration::days(7);

/// A feed bigger than this is not our appcast. It is a few KB today.
pub const MAX_FEED_BYTES: usize = 1 << 20;

/// One release, as the feed lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    pub published: DateTime<Utc>,
}

/// What to do about the newest release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Nothing yet: already announced, or still inside [`GRACE`].
    Wait,
    /// The account already runs it. Record it as announced; push nothing.
    AlreadyHave,
    /// Push it. Macs always; phones only when `phones`.
    Announce { phones: bool },
}

/// Dotted digits, compared numerically: `0.0.10` is newer than `0.0.9`.
/// `None` for anything else.
pub fn parse_version(v: &str) -> Option<Vec<u32>> {
    let parts = v
        .trim()
        .split('.')
        .map(|p| p.parse::<u32>().ok())
        .collect::<Option<Vec<_>>>()?;
    (2..=4).contains(&parts.len()).then_some(parts)
}

/// `a` is at least `b`. Unparseable on either side is `false`, which errs
/// towards announcing rather than towards silence only where that is safe:
/// the caller treats an unparseable feed as no release at all.
pub fn at_least(a: &str, b: &str) -> bool {
    match (parse_version(a), parse_version(b)) {
        (Some(a), Some(b)) => {
            let n = a.len().max(b.len());
            let pad = |v: &Vec<u32>| {
                (0..n)
                    .map(|i| v.get(i).copied().unwrap_or(0))
                    .collect::<Vec<_>>()
            };
            pad(&a) >= pad(&b)
        }
        _ => false,
    }
}

static ITEM: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)<item>(.*?)</item>").unwrap());
static VERSION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"<sparkle:shortVersionString>\s*([0-9.]{3,32})\s*</sparkle:shortVersionString>")
        .unwrap()
});
static PUB_DATE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<pubDate>\s*([^<]{1,64}?)\s*</pubDate>").unwrap());

/// The newest release in a Sparkle appcast, by version. An item missing its
/// version or date, or carrying one that does not parse, is skipped: the feed
/// is ours, but it still arrives over a network, and a half-written item must
/// not become a push.
pub fn newest(feed: &str) -> Option<Release> {
    ITEM.captures_iter(feed)
        .filter_map(|item| {
            let body = item.get(1)?.as_str();
            let version = VERSION.captures(body)?.get(1)?.as_str().to_string();
            parse_version(&version)?;
            let date = PUB_DATE.captures(body)?.get(1)?.as_str();
            let published = DateTime::parse_from_rfc2822(date).ok()?.with_timezone(&Utc);
            Some(Release { version, published })
        })
        .reduce(|best, r| {
            if at_least(&best.version, &r.version) {
                best
            } else {
                r
            }
        })
}

/// Who hears about `release`; see the module header.
pub fn decide(
    release: &Release,
    now: DateTime<Utc>,
    seen: &[ClientSeen],
    announced: Option<&str>,
) -> Decision {
    if announced.is_some_and(|a| at_least(a, &release.version)) {
        return Decision::Wait;
    }
    let macs = seen.iter().filter(|c| c.platform == ClientPlatform::Macos);
    if macs.clone().any(|c| at_least(&c.version, &release.version)) {
        return Decision::AlreadyHave;
    }
    if now - release.published < GRACE {
        return Decision::Wait;
    }
    let mut macs = macs.peekable();
    let had_mac = macs.peek().is_some();
    let mac_active = macs.any(|c| now - c.seen_at < MAC_ACTIVE);
    Decision::Announce {
        phones: had_mac && !mac_active,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FEED: &str = r#"<?xml version="1.0"?>
<rss xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle" version="2.0">
    <channel>
        <title>Passband</title>
        <item>
            <title>0.0.9</title>
            <pubDate>Wed, 04 Nov 2026 03:22:34 +0000</pubDate>
            <sparkle:shortVersionString>0.0.9</sparkle:shortVersionString>
        </item>
        <item>
            <title>0.0.10</title>
            <pubDate>Thu, 05 Nov 2026 03:22:34 +0000</pubDate>
            <sparkle:version>2605</sparkle:version>
            <sparkle:shortVersionString>0.0.10</sparkle:shortVersionString>
        </item>
        <item>
            <title>broken</title>
            <sparkle:shortVersionString>9.9.9</sparkle:shortVersionString>
        </item>
    </channel>
</rss>"#;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn mac(version: &str, seen_at: &str) -> ClientSeen {
        ClientSeen {
            platform: ClientPlatform::Macos,
            version: version.into(),
            seen_at: at(seen_at),
        }
    }

    fn release() -> Release {
        Release {
            version: "0.0.10".into(),
            published: at("2026-11-05T03:22:34Z"),
        }
    }

    #[test]
    fn versions_compare_as_numbers() {
        assert!(at_least("0.0.10", "0.0.9"));
        assert!(!at_least("0.0.9", "0.0.10"));
        assert!(at_least("0.1", "0.0.99"));
        assert!(at_least("1.0", "1.0.0"));
        assert!(!at_least("junk", "0.0.1"));
        assert!(!at_least("0.0.1", "junk"));
        assert_eq!(parse_version("1"), None);
        assert_eq!(parse_version("1.2.3.4.5"), None);
    }

    #[test]
    fn newest_is_by_version_and_skips_broken_items() {
        // The undated 9.9.9 item is not a release.
        assert_eq!(newest(FEED), Some(release()));
        assert_eq!(newest("<rss><channel></channel></rss>"), None);
        assert_eq!(newest("not xml at all"), None);
    }

    #[test]
    fn the_real_appcast_parses() {
        let feed = include_str!("../../passband-site/appcast.xml");
        let r = newest(feed).expect("the shipped appcast has a release");
        assert!(parse_version(&r.version).is_some());
    }

    #[test]
    fn nothing_inside_the_grace_period() {
        let now = release().published + Duration::hours(5);
        assert_eq!(decide(&release(), now, &[], None), Decision::Wait);
    }

    #[test]
    fn a_lapsed_account_hears_on_every_device() {
        let now = release().published + Duration::hours(7);
        let seen = [mac("0.0.9", "2026-10-01T00:00:00Z")];
        assert_eq!(
            decide(&release(), now, &seen, None),
            Decision::Announce { phones: true }
        );
        // Phone only, never a Mac: a Mac release is not news for the phone.
        assert_eq!(
            decide(&release(), now, &[], None),
            Decision::Announce { phones: false }
        );
    }

    #[test]
    fn an_active_mac_keeps_the_phone_quiet() {
        let now = release().published + Duration::hours(7);
        let seen = [mac("0.0.9", "2026-11-05T09:00:00Z")];
        assert_eq!(
            decide(&release(), now, &seen, None),
            Decision::Announce { phones: false }
        );
    }

    #[test]
    fn a_mac_already_on_it_ends_it_even_inside_the_grace() {
        let now = release().published + Duration::hours(1);
        let seen = [
            mac("0.0.9", "2026-11-01T00:00:00Z"),
            mac("0.0.10", "2026-11-05T04:00:00Z"),
        ];
        assert_eq!(decide(&release(), now, &seen, None), Decision::AlreadyHave);
        let newer = [mac("0.0.11", "2026-11-05T04:00:00Z")];
        assert_eq!(decide(&release(), now, &newer, None), Decision::AlreadyHave);
    }

    #[test]
    fn once_per_version() {
        let now = release().published + Duration::days(3);
        assert_eq!(decide(&release(), now, &[], Some("0.0.10")), Decision::Wait);
        assert_eq!(decide(&release(), now, &[], Some("0.0.11")), Decision::Wait);
        assert_eq!(
            decide(&release(), now, &[], Some("0.0.9")),
            Decision::Announce { phones: false }
        );
    }
}
