//! TELLING A MAC A NEW VERSION IS OUT, even when Passband is not running.
//!
//! The Mac app announces its own updates (Sparkle, then a local banner), but
//! only while it runs. A Mac where Passband was quit hears nothing. So each
//! daemon reads the PUBLIC release feed (the Mac's Sparkle appcast, published
//! the moment a release is downloadable) and, once per version, pushes a
//! release banner to the Macs it holds tokens for. The relay stays blind: it
//! has no device registry, so the daemon that owns the tokens is the only
//! place a fan-out like this can start. Macs only: a phone is never told about
//! a Mac release.
//!
//! WHO HEARS IT ([`decide`]):
//! - Nobody, while the news is younger than [`GRACE`]: a running Mac picks
//!   the update up on its own within the hour, and its own banner says so.
//! - Nobody, ever, once any of the account's Macs has connected running the
//!   news (or newer). They have it.
//! - Every registered Mac otherwise. A Mac that is running swallows the push
//!   (its Sparkle banner covers it); a Mac where Passband is quit shows it.
//!
//! QUIET RELEASES. A release whose tag message is `quiet` (a hotfix) carries
//! `<passband:quiet>` in the appcast (passband/make-appcast.sh). It is never
//! news by itself, but it does not swallow news either: a feature release
//! still inside its grace when a quiet fix lands is announced, naming the
//! newest version, since that is what the Mac will install. A run of quiet
//! releases with no news among them is recorded and nobody hears.
//!
//! FIRST SIGHT IS NOT NEWS. With no record at all (a daemon running this code
//! for the first time), the current release is recorded and nobody is pushed:
//! the first announcement is the first release that comes out AFTER this
//! shipped, not a backlog the moment the daemon rolls.
//!
//! One banner per daemon: a Mac with mailboxes on two daemons hears it twice,
//! since each daemon decides alone. The shared collapse id folds them into
//! one Notification Center entry.
//!
//! ONCE PER VERSION: the newest version announced (or found already
//! installed, or passed over as quiet) is kept in `app_settings` under
//! [`ANNOUNCED_KEY`], so a restart does not repeat it and a release that was
//! skipped is never sent later.

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

/// A feed bigger than this is not our appcast. It is a few KB today.
pub const MAX_FEED_BYTES: usize = 1 << 20;

/// One release, as the feed lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    pub published: DateTime<Utc>,
    /// Tagged quiet: never news by itself.
    pub quiet: bool,
}

/// What to do about the feed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Nothing yet: nothing new, or the news is still inside [`GRACE`].
    Wait,
    /// Record this version as announced and push nothing: the account already
    /// runs the news, everything new was quiet, or this is the first check.
    Record(String),
    /// Push this version (the newest) to the Macs.
    Announce(String),
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

static QUIET: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<passband:quiet\b").unwrap());

/// Every release in a Sparkle appcast. An item missing its version or date,
/// or carrying one that does not parse, is skipped: the feed is ours, but it
/// still arrives over a network, and a half-written item must not become a
/// push.
pub fn releases(feed: &str) -> Vec<Release> {
    ITEM.captures_iter(feed)
        .filter_map(|item| {
            let body = item.get(1)?.as_str();
            let version = VERSION.captures(body)?.get(1)?.as_str().to_string();
            parse_version(&version)?;
            let date = PUB_DATE.captures(body)?.get(1)?.as_str();
            let published = DateTime::parse_from_rfc2822(date).ok()?.with_timezone(&Utc);
            Some(Release {
                version,
                published,
                quiet: QUIET.is_match(body),
            })
        })
        .collect()
}

fn newest<'a>(rs: impl Iterator<Item = &'a Release>) -> Option<&'a Release> {
    rs.reduce(|best, r| {
        if at_least(&best.version, &r.version) {
            best
        } else {
            r
        }
    })
}

/// Who hears about the feed's releases; see the module header.
pub fn decide(
    releases: &[Release],
    now: DateTime<Utc>,
    seen: &[ClientSeen],
    announced: Option<&str>,
) -> Decision {
    let Some(latest) = newest(releases.iter()) else {
        return Decision::Wait;
    };
    let Some(announced) = announced else {
        return Decision::Record(latest.version.clone());
    };
    if at_least(announced, &latest.version) {
        return Decision::Wait;
    }
    let news = newest(
        releases
            .iter()
            .filter(|r| !r.quiet && !at_least(announced, &r.version)),
    );
    let Some(news) = news else {
        return Decision::Record(latest.version.clone());
    };
    let has_it = seen
        .iter()
        .any(|c| c.platform == ClientPlatform::Macos && at_least(&c.version, &news.version));
    if has_it {
        return Decision::Record(latest.version.clone());
    }
    if now - news.published < GRACE {
        return Decision::Wait;
    }
    Decision::Announce(latest.version.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    const FEED: &str = r#"<?xml version="1.0"?>
<rss xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle" xmlns:passband="https://passband.app/xml-namespaces/appcast" version="2.0">
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
            <passband:quiet>yes</passband:quiet>
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

    fn rel(version: &str, published: &str, quiet: bool) -> Release {
        Release {
            version: version.into(),
            published: at(published),
            quiet,
        }
    }

    /// 0.0.10, a feature release.
    fn loud() -> Release {
        rel("0.0.10", "2026-11-05T03:22:34Z", false)
    }

    const OLD: Option<&str> = Some("0.0.9");

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
    fn the_feed_parses_with_its_quiet_marks_and_skips_broken_items() {
        // The undated 9.9.9 item is not a release.
        assert_eq!(
            releases(FEED),
            vec![
                rel("0.0.9", "2026-11-04T03:22:34Z", false),
                rel("0.0.10", "2026-11-05T03:22:34Z", true),
            ]
        );
        assert!(releases("<rss><channel></channel></rss>").is_empty());
        assert!(releases("not xml at all").is_empty());
    }

    #[test]
    fn the_real_appcast_parses() {
        let feed = include_str!("../../passband-site/appcast.xml");
        assert!(
            !releases(feed).is_empty(),
            "the shipped appcast has a release"
        );
    }

    #[test]
    fn nothing_inside_the_grace_period() {
        let now = loud().published + Duration::hours(5);
        assert_eq!(decide(&[loud()], now, &[], OLD), Decision::Wait);
    }

    #[test]
    fn after_the_grace_every_mac_hears() {
        let now = loud().published + Duration::hours(7);
        let seen = [mac("0.0.9", "2026-10-01T00:00:00Z")];
        assert_eq!(
            decide(&[loud()], now, &seen, OLD),
            Decision::Announce("0.0.10".into())
        );
    }

    #[test]
    fn the_first_check_ever_records_and_pushes_nothing() {
        let now = loud().published + Duration::days(3);
        assert_eq!(
            decide(&[loud()], now, &[], None),
            Decision::Record("0.0.10".into())
        );
    }

    #[test]
    fn a_mac_already_on_it_ends_it_even_inside_the_grace() {
        let now = loud().published + Duration::hours(1);
        let seen = [
            mac("0.0.9", "2026-11-01T00:00:00Z"),
            mac("0.0.10", "2026-11-05T04:00:00Z"),
        ];
        assert_eq!(
            decide(&[loud()], now, &seen, OLD),
            Decision::Record("0.0.10".into())
        );
        let newer = [mac("0.0.11", "2026-11-05T04:00:00Z")];
        assert_eq!(
            decide(&[loud()], now, &newer, OLD),
            Decision::Record("0.0.10".into())
        );
    }

    #[test]
    fn once_per_version() {
        let now = loud().published + Duration::days(3);
        assert_eq!(decide(&[loud()], now, &[], Some("0.0.10")), Decision::Wait);
        assert_eq!(decide(&[loud()], now, &[], Some("0.0.11")), Decision::Wait);
        assert_eq!(
            decide(&[loud()], now, &[], Some("0.0.9")),
            Decision::Announce("0.0.10".into())
        );
    }

    #[test]
    fn a_quiet_release_alone_is_recorded_not_announced() {
        let fix = rel("0.0.11", "2026-11-06T00:00:00Z", true);
        let now = fix.published + Duration::days(1);
        assert_eq!(
            decide(&[loud(), fix], now, &[], Some("0.0.10")),
            Decision::Record("0.0.11".into())
        );
    }

    #[test]
    fn a_quiet_fix_does_not_swallow_the_news_before_it() {
        // 0.0.10 is news, still in its grace when quiet 0.0.11 lands.
        let fix = rel("0.0.11", "2026-11-05T05:00:00Z", true);
        let early = loud().published + Duration::hours(3);
        assert_eq!(
            decide(&[loud(), fix.clone()], early, &[], OLD),
            Decision::Wait
        );
        // The grace runs from the NEWS, and the push names the newest build.
        let later = loud().published + Duration::hours(7);
        assert_eq!(
            decide(&[loud(), fix.clone()], later, &[], OLD),
            Decision::Announce("0.0.11".into())
        );
        // A Mac that took the news (but not the fix yet) is done.
        let seen = [mac("0.0.10", "2026-11-05T04:00:00Z")];
        assert_eq!(
            decide(&[loud(), fix], later, &seen, OLD),
            Decision::Record("0.0.11".into())
        );
    }
}
