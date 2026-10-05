//! How tired of a sender the owner's own behaviour says they are.
//!
//! PURE: counts in, a score and its reasons out. No store, no clock, no
//! network. The Reading purge flow walks senders in score order and suggests
//! unsubscribing, and it has to be able to say WHY, so every point of the
//! score is attributable to a [`Reason`] the client can turn into a sentence
//! ("opened 1 of 23"). The daemon never writes that sentence: reasons are a
//! code plus the numbers, and the copy lives with the client.
//!
//! WHAT THE SCORE IS NOT. It is not a judgement of the sender, only of what
//! this owner did with their mail in this client. Opens, dwell and clicks are
//! recorded by Passband alone, so mail read in Gmail on a phone looks ignored
//! here. That biases every score UP, which is why the flow built on it only
//! ever suggests, and why a person the owner writes to is exempt outright.
//!
//! DWELL IS JUDGED AGAINST LENGTH, not as a number of seconds. Four seconds on
//! a thirty-word promo is a complete read and must not count against its
//! sender; four seconds on a two-thousand-word essay is a skim. So each opened
//! message carries a READ FRACTION (time spent over the time its words would
//! take), and only mail too short or too image-heavy to have one falls back to
//! an absolute threshold, at a lower weight because without a length we know
//! less.

use serde::{Deserialize, Serialize};

/// Below this many received messages there is no pattern to read: one ignored
/// welcome mail is not fatigue. The score is `None`, not zero, so the client
/// can tell "nothing against them" from "nothing known".
pub const MIN_SAMPLE: u32 = 4;

/// How many days of ledger a score needs. Opens are only recorded from the
/// day the ledger starts, so on a ledger a day old EVERYTHING is unopened and
/// a handful of mails cleared in one sitting reads as months of indifference.
/// Under two weeks there is no pattern, only a first impression: no score,
/// for anyone, and so nothing for smart done to sweep a sender on.
pub const MIN_COVERED_DAYS: f32 = 14.0;

/// How many days of ledger a monthly rate needs. Four mails in three hours is
/// not "960 a month"; under a week the rate is not stated at all, and the
/// volume term with it.
pub const MIN_RATE_DAYS: f32 = 7.0;

/// The monthly rate `received` mails in `covered_days` amount to, to one
/// decimal, or `None` under [`MIN_RATE_DAYS`].
pub fn per_month(received: u32, covered_days: f32) -> Option<f32> {
    (covered_days >= MIN_RATE_DAYS)
        .then(|| (received as f32 * 30.0 / covered_days * 10.0).round() / 10.0)
}

/// The sample size at which the ignore term reaches full weight. Four unopened
/// mails are evidence; eight are a habit. Between the two the term is damped
/// linearly so a brand-new subscription cannot outrank one ignored for months.
pub const FULL_WEIGHT_SAMPLE: u32 = 8;

/// Silent adult reading speed for English non-fiction, in words per minute
/// (Brysbaert 2019, a meta-analysis of 190 studies). The yardstick a dwell is
/// measured against. It is deliberately the UNHURRIED figure: someone who
/// reads faster than this simply saturates at a read fraction of 1.0, and the
/// thresholds below sit far enough under it (10% and 30%) that no plausible
/// reading speed turns a real read into a skim.
pub const READING_WPM: u32 = 238;

/// A message with fewer words than this has no read fraction. An image-only
/// promo or a one-line notice is taken in at a glance, so a short look at it
/// is a complete read rather than a skim, and dividing a few seconds by an
/// expected second or two would only manufacture noise.
pub const MIN_WORDS_FOR_READ_FRACTION: u32 = 40;

/// How long a "keep" from the purge flow holds. The owner looked at this
/// sender and chose to stay subscribed; asking again next week would make the
/// choice worthless, and never asking again would make it permanent on the
/// strength of one tap. Two months is long enough to forget the question and
/// short enough that a subscription gone stale comes back up.
pub const KEEP_QUIET_DAYS: i64 = 60;

/// Is a keep made at `kept_at` still the owner's latest word on a sender?
///
/// Not once [`KEEP_QUIET_DAYS`] have passed, and not when the owner has since
/// said the opposite: asked the sender to stop (`unsubscribed_at`) or written
/// a squelch rule that governs them (`squelched_at`). THE NEWER DECISION WINS,
/// decided here at read time, so the keep row is never deleted and no rule or
/// unsubscribe writer has to know keeps exist. A keep made AFTER either of
/// those is the newer decision and holds.
pub fn keep_in_force(
    kept_at: chrono::DateTime<chrono::Utc>,
    now: chrono::DateTime<chrono::Utc>,
    unsubscribed_at: Option<chrono::DateTime<chrono::Utc>>,
    squelched_at: Option<chrono::DateTime<chrono::Utc>>,
) -> bool {
    now - kept_at < chrono::Duration::days(KEEP_QUIET_DAYS)
        && unsubscribed_at.is_none_or(|at| at <= kept_at)
        && squelched_at.is_none_or(|at| at <= kept_at)
}

/// How many opened messages a dwell statistic needs before it says anything.
const MIN_ATTENTION_SAMPLE: u32 = 3;

/// How many opened messages "never clicked" needs. Higher than the dwell floor
/// because a click is rare even in mail people like.
const MIN_CLICK_SAMPLE: u32 = 5;

/// What the store counted for one sender. Every count is over the same
/// universe: inbound, not provider spam, not sealed, not the owner's own mail,
/// received inside the stats window.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct SenderSignals {
    /// Messages received in the window.
    pub received: u32,
    /// Of those, the ones with `opened_at` set.
    pub opened: u32,
    /// Of those, marked done without ever being opened.
    pub cleared_unopened: u32,
    /// Opened at or after the dwell ledger floor: the only opens whose dwell
    /// and clicks mean anything.
    pub dwell_sample: u32,
    /// Median dwell over `dwell_sample`; `None` when it is empty.
    pub median_dwell_ms: Option<u64>,
    /// Link clicks summed over `dwell_sample`.
    pub clicks: u32,
    /// The part of `dwell_sample` long enough to have a read fraction.
    pub read_sample: u32,
    /// Median read fraction over `read_sample`, 0.0..=1.0.
    pub median_read_fraction: Option<f32>,
    /// Median word count over `read_sample`.
    pub median_words: Option<u32>,
    /// `received` scaled to thirty days. `None` when the window is shorter
    /// than [`MIN_RATE_DAYS`]: there is no monthly rate in three days of mail.
    pub per_month: Option<f32>,
    /// How many days of recorded opens the counts above actually cover.
    pub covered_days: f32,
    /// The owner has written to this address.
    pub known_contact: bool,
    /// A rule with disposition `surface` governs the sender.
    pub surface_rule: bool,
    /// The owner chose to keep this sender, and that is still their latest
    /// word ([`keep_in_force`]).
    pub kept_recently: bool,
}

/// One attributable part of a score. Tagged with a snake_case `code` and
/// carrying only numbers: the client owns the sentence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum Reason {
    /// Fewer than [`MIN_SAMPLE`] messages: no score.
    NotEnoughHistory,
    /// The owner writes to this address: never a purge suggestion.
    KnownContact,
    /// The owner wrote a rule asking for this sender's mail.
    WantedByRule,
    /// The owner was asked about this sender lately and chose to keep them.
    KeptRecently,
    RarelyOpened {
        opened: u32,
        received: u32,
    },
    ClearedUnopened {
        count: u32,
    },
    /// Opened, but for a small part of the time its length would take.
    Skimmed {
        median_read_fraction: f32,
        median_words: u32,
        median_dwell_ms: u64,
    },
    /// Opened briefly, with no length to judge the look against.
    BriefLooks {
        median_dwell_ms: u64,
    },
    /// Opened and read through. The one reason that argues for KEEPING.
    ReadClosely {
        median_read_fraction: f32,
    },
    NeverClicked {
        opened: u32,
    },
    HighVolume {
        per_month: f32,
    },
}

/// One opened message, as the store hands it over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenSample {
    pub dwell_ms: u64,
    pub link_clicks: u32,
    /// Whitespace-separated words in the flattened body triage reads.
    pub words: u32,
}

/// What a sender's opened mail adds up to: the dwell half of [`SenderSignals`].
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct AttentionSummary {
    pub dwell_sample: u32,
    pub median_dwell_ms: Option<u64>,
    pub clicks: u32,
    pub read_sample: u32,
    pub median_read_fraction: Option<f32>,
    pub median_words: Option<u32>,
}

/// Whitespace-separated words. Counted over the flattened plain text ingest
/// stores in `messages.body`, the same text triage reads, so markup, tracking
/// URLs in attributes and hidden preheaders never inflate a length.
pub fn word_count(text: &str) -> u32 {
    u32::try_from(text.split_whitespace().count()).unwrap_or(u32::MAX)
}

/// How much of a message the owner had time to read: dwell over the time its
/// words take at [`READING_WPM`], clamped to 0.0..=1.0. `None` below
/// [`MIN_WORDS_FOR_READ_FRACTION`], where the question has no answer.
pub fn read_fraction(words: u32, dwell_ms: u64) -> Option<f32> {
    if words < MIN_WORDS_FOR_READ_FRACTION {
        return None;
    }
    let expected_ms = f64::from(words) * 60_000.0 / f64::from(READING_WPM);
    Some((dwell_ms as f64 / expected_ms).clamp(0.0, 1.0) as f32)
}

/// Fold a sender's opened messages into the medians the score reads.
pub fn summarize_attention(samples: &[OpenSample]) -> AttentionSummary {
    let mut dwell: Vec<u64> = samples.iter().map(|s| s.dwell_ms).collect();
    let read: Vec<(f32, u32)> = samples
        .iter()
        .filter_map(|s| read_fraction(s.words, s.dwell_ms).map(|f| (f, s.words)))
        .collect();
    let mut fractions: Vec<f32> = read.iter().map(|r| r.0).collect();
    let mut words: Vec<u64> = read.iter().map(|r| u64::from(r.1)).collect();
    fractions.sort_by(f32::total_cmp);
    let median_read_fraction = match fractions.len() {
        0 => None,
        n if n % 2 == 1 => Some(fractions[n / 2]),
        n => Some((fractions[n / 2 - 1] + fractions[n / 2]) / 2.0),
    }
    // Three decimals: enough to order senders, and it keeps the wire value
    // from carrying float noise the client would have to round away.
    .map(|f| (f * 1000.0).round() / 1000.0);
    AttentionSummary {
        dwell_sample: samples.len() as u32,
        median_dwell_ms: median(&mut dwell),
        clicks: samples
            .iter()
            .fold(0u32, |sum, s| sum.saturating_add(s.link_clicks)),
        read_sample: read.len() as u32,
        median_read_fraction,
        median_words: median(&mut words).map(|w| w as u32),
    }
}

/// The median, averaging the middle pair of an even count. Sorts in place.
pub fn median(values: &mut [u64]) -> Option<u64> {
    values.sort_unstable();
    match values.len() {
        0 => None,
        n if n % 2 == 1 => Some(values[n / 2]),
        n => Some(
            values[n / 2 - 1] / 2
                + values[n / 2] / 2
                + (values[n / 2 - 1] % 2 + values[n / 2] % 2) / 2,
        ),
    }
}

/// Score one sender, 0 (keep) to 100 (the owner has plainly stopped reading
/// them), with the reasons ordered by how much each contributed.
pub fn score(signals: &SenderSignals) -> (Option<u8>, Vec<Reason>) {
    // FIRST, before the sample floor: a person the owner writes to is never a
    // purge suggestion (product rule), and "not enough history" would leave a
    // client free to offer them anyway. A `surface` rule is the same statement
    // made deliberately. Both are a flat zero with one reason, whatever the
    // rest of the counts say.
    if signals.known_contact {
        return (Some(0), vec![Reason::KnownContact]);
    }
    if signals.surface_rule {
        return (Some(0), vec![Reason::WantedByRule]);
    }
    if signals.kept_recently {
        return (Some(0), vec![Reason::KeptRecently]);
    }
    // TOO FEW MAILS, OR TOO FEW DAYS. The second is the one a young ledger
    // trips: plenty of mail, and no time for any of it to have been opened.
    if signals.received < MIN_SAMPLE || signals.covered_days < MIN_COVERED_DAYS {
        return (None, vec![Reason::NotEnoughHistory]);
    }

    let received = signals.received as f32;
    // A store that counted more opens than mail has a bug; it must not turn
    // into a negative term here.
    let opened = signals.opened.min(signals.received);
    let mut parts: Vec<(f32, Reason)> = Vec::new();

    // IGNORED: the share never opened, damped while the sample is small.
    let ignore =
        55.0 * (1.0 - opened as f32 / received) * (received / FULL_WEIGHT_SAMPLE as f32).min(1.0);
    let ignore_reason = Reason::RarelyOpened {
        opened,
        received: signals.received,
    };
    let mut total = ignore;
    if ignore >= 20.0 {
        parts.push((ignore, ignore_reason));
    }

    // CLEARED WITHOUT OPENING: a decision, where merely unopened is an absence.
    let cleared = 15.0 * signals.cleared_unopened as f32 / received;
    total += cleared;
    if signals.cleared_unopened >= 2 {
        parts.push((
            cleared,
            Reason::ClearedUnopened {
                count: signals.cleared_unopened,
            },
        ));
    }

    // HOW IT WAS READ WHEN IT WAS OPENED. One term, two sources, never both:
    // the read fraction when enough of the opened mail is long enough to have
    // one, else the absolute look. The fallback tops out at 8 where a skim
    // reaches 15, because a short look at mail of unknown length is weaker
    // evidence than a short look at mail known to be long.
    let read_fraction = signals
        .median_read_fraction
        .filter(|_| signals.read_sample >= MIN_ATTENTION_SAMPLE);
    if let Some(fraction) = read_fraction {
        let skimmed = if fraction < 0.10 {
            15.0
        } else if fraction < 0.30 {
            8.0
        } else {
            0.0
        };
        if skimmed > 0.0 {
            total += skimmed;
            parts.push((
                skimmed,
                Reason::Skimmed {
                    median_read_fraction: fraction,
                    median_words: signals.median_words.unwrap_or(0),
                    median_dwell_ms: signals.median_dwell_ms.unwrap_or(0),
                },
            ));
        } else if fraction >= 0.6 {
            // The one NEGATIVE term: mail that is read through earns its
            // place, and the flow can say so instead of only ever accusing.
            total -= 15.0;
            parts.push((
                -15.0,
                Reason::ReadClosely {
                    median_read_fraction: fraction,
                },
            ));
        }
    } else if signals.dwell_sample >= MIN_ATTENTION_SAMPLE
        && let Some(dwell) = signals.median_dwell_ms
        && dwell < 3_000
    {
        total += 8.0;
        parts.push((
            8.0,
            Reason::BriefLooks {
                median_dwell_ms: dwell,
            },
        ));
    }

    // NEVER FOLLOWED A LINK, over enough opens for that to be a pattern.
    if signals.dwell_sample >= MIN_CLICK_SAMPLE && signals.clicks == 0 {
        total += 5.0;
        parts.push((
            5.0,
            Reason::NeverClicked {
                opened: signals.dwell_sample,
            },
        ));
    }

    // VOLUME: the same indifference costs more attention at twenty a month.
    if let Some(per_month) = signals.per_month {
        let volume = if per_month >= 20.0 {
            10.0
        } else if per_month >= 8.0 {
            5.0
        } else {
            0.0
        };
        if volume > 0.0 {
            total += volume;
            parts.push((volume, Reason::HighVolume { per_month }));
        }
    }

    // Stable, so equal contributions keep the order they were weighed in.
    parts.sort_by(|a, b| b.0.total_cmp(&a.0));
    let score = total.round().clamp(0.0, 100.0) as u8;
    (Some(score), parts.into_iter().map(|p| p.1).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sender with history and nothing else said about them.
    fn base(received: u32, opened: u32) -> SenderSignals {
        SenderSignals {
            received,
            opened,
            // A ledger that has been running the whole window.
            covered_days: 90.0,
            ..Default::default()
        }
    }

    fn codes(reasons: &[Reason]) -> Vec<String> {
        reasons
            .iter()
            .map(|r| {
                serde_json::to_value(r).unwrap()["code"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn too_little_history_has_no_score() {
        let (score, reasons) = score(&base(3, 0));
        assert_eq!(score, None);
        assert_eq!(reasons, vec![Reason::NotEnoughHistory]);
        assert!(super::score(&base(4, 0)).0.is_some(), "four is enough");
    }

    #[test]
    fn a_known_contact_is_zero_whatever_else_is_true() {
        let mut s = base(40, 0);
        s.cleared_unopened = 40;
        s.per_month = Some(40.0);
        s.known_contact = true;
        assert_eq!(score(&s), (Some(0), vec![Reason::KnownContact]));
        // Even with too little history: "no score" would leave the client free
        // to suggest them.
        let mut few = base(1, 0);
        few.known_contact = true;
        assert_eq!(score(&few), (Some(0), vec![Reason::KnownContact]));
    }

    #[test]
    fn a_young_ledger_has_no_score_whatever_the_counts() {
        // Eight ignored, six cleared unopened, on thirty hours of ledger.
        let mut s = base(8, 0);
        s.cleared_unopened = 6;
        s.covered_days = 1.25;
        assert_eq!(score(&s), (None, vec![Reason::NotEnoughHistory]));
        s.covered_days = 13.9;
        assert_eq!(score(&s).0, None);
        s.covered_days = 14.0;
        assert_eq!(score(&s).0, Some(66), "55 + 15 * 6/8");
        // The owner's own word still answers first.
        s.covered_days = 1.0;
        s.known_contact = true;
        assert_eq!(score(&s), (Some(0), vec![Reason::KnownContact]));
    }

    #[test]
    fn a_monthly_rate_needs_a_week_of_ledger() {
        assert_eq!(per_month(4, 0.125), None, "four in three hours");
        assert_eq!(per_month(8, 6.9), None);
        assert_eq!(per_month(7, 7.0), Some(30.0));
        assert_eq!(per_month(5, 10.0), Some(15.0));
        // And with no rate there is no volume term and no reason for one.
        let mut s = base(8, 8);
        s.per_month = None;
        assert_eq!(score(&s), (Some(0), vec![]));
    }

    #[test]
    fn a_surface_rule_is_zero_with_its_own_reason() {
        let mut s = base(40, 0);
        s.surface_rule = true;
        assert_eq!(score(&s), (Some(0), vec![Reason::WantedByRule]));
    }

    #[test]
    fn a_recent_keep_is_zero_with_its_own_reason() {
        let mut s = base(40, 0);
        s.kept_recently = true;
        assert_eq!(score(&s), (Some(0), vec![Reason::KeptRecently]));
        assert_eq!(
            serde_json::to_value(Reason::KeptRecently).unwrap(),
            serde_json::json!({"code": "kept_recently"})
        );
    }

    #[test]
    fn a_keep_holds_for_sixty_days_unless_a_newer_decision_overrides_it() {
        use chrono::{Duration, TimeZone, Utc};
        let now = Utc.with_ymd_and_hms(2026, 10, 5, 12, 0, 0).unwrap();
        let kept = now - Duration::days(10);
        assert!(keep_in_force(kept, now, None, None));
        assert!(keep_in_force(
            now - Duration::days(60) + Duration::seconds(1),
            now,
            None,
            None
        ));
        assert!(!keep_in_force(now - Duration::days(60), now, None, None));
        // An unsubscribe or a squelch rule AFTER the keep outranks it.
        let later = Some(kept + Duration::days(1));
        assert!(!keep_in_force(kept, now, later, None));
        assert!(!keep_in_force(kept, now, None, later));
        // One from BEFORE it does not: the keep is the newer decision.
        let earlier = Some(kept - Duration::days(1));
        assert!(keep_in_force(kept, now, earlier, earlier));
    }

    #[test]
    fn everything_opened_scores_low() {
        let (score, reasons) = score(&base(20, 20));
        assert_eq!(score, Some(0));
        assert!(reasons.is_empty(), "{reasons:?}");
    }

    #[test]
    fn nothing_opened_across_eight_scores_the_full_ignore_term() {
        let (score, reasons) = score(&base(8, 0));
        assert_eq!(score, Some(55));
        assert_eq!(
            reasons,
            vec![Reason::RarelyOpened {
                opened: 0,
                received: 8
            }]
        );
    }

    #[test]
    fn a_small_sample_is_damped() {
        // Four unopened of four: half weight, 27.5.
        assert_eq!(score(&base(4, 0)).0, Some(28));
        // And the reason needs a contribution of at least 20: five unopened of
        // sixteen is 55 * 5/16 = 17, which scores but does not accuse.
        let (score, reasons) = score(&base(16, 11));
        assert_eq!(score, Some(17));
        assert!(reasons.is_empty(), "{reasons:?}");
    }

    #[test]
    fn more_opens_than_mail_cannot_go_negative() {
        // Unguarded, the ignore term would be 55 * (1 - 10) and swallow the
        // cleared term whole before the final clamp hid it.
        let mut s = base(8, 80);
        s.cleared_unopened = 8;
        assert_eq!(score(&s).0, Some(15));
    }

    #[test]
    fn cleared_unopened_adds_its_share_and_names_itself_from_two() {
        let mut s = base(10, 10);
        s.cleared_unopened = 1;
        let (one, reasons) = score(&s);
        assert_eq!(one, Some(2), "15 * 1/10, rounded");
        assert!(reasons.is_empty());
        s.cleared_unopened = 10;
        let (all, reasons) = score(&s);
        assert_eq!(all, Some(15));
        assert_eq!(reasons, vec![Reason::ClearedUnopened { count: 10 }]);
    }

    #[test]
    fn long_mail_skimmed_scores() {
        let mut s = base(10, 10);
        s.dwell_sample = 3;
        s.read_sample = 3;
        s.median_dwell_ms = Some(4_000);
        s.median_words = Some(1_500);
        s.median_read_fraction = Some(0.01);
        let (score, reasons) = super::score(&s);
        assert_eq!(score, Some(15));
        assert_eq!(
            reasons,
            vec![Reason::Skimmed {
                median_read_fraction: 0.01,
                median_words: 1_500,
                median_dwell_ms: 4_000
            }]
        );
        s.median_read_fraction = Some(0.2);
        assert_eq!(super::score(&s).0, Some(8), "a partial read costs less");
        s.median_read_fraction = Some(0.4);
        assert_eq!(super::score(&s), (Some(0), vec![]), "half read is neither");
    }

    #[test]
    fn the_read_fraction_needs_three_messages() {
        let mut s = base(10, 10);
        s.dwell_sample = 2;
        s.read_sample = 2;
        s.median_dwell_ms = Some(60_000);
        s.median_read_fraction = Some(0.01);
        assert_eq!(score(&s), (Some(0), vec![]));
    }

    #[test]
    fn short_mail_with_a_short_look_does_not_count_against_the_sender() {
        // Three thirty-word promos, four seconds each: read in full. No read
        // fraction exists (too short), and four seconds is not under the
        // absolute fallback's three.
        let samples = [OpenSample {
            dwell_ms: 4_000,
            link_clicks: 0,
            words: 30,
        }; 3];
        let a = summarize_attention(&samples);
        assert_eq!(a.read_sample, 0);
        assert_eq!(a.median_read_fraction, None);
        let mut s = base(10, 10);
        s.dwell_sample = a.dwell_sample;
        s.median_dwell_ms = a.median_dwell_ms;
        s.read_sample = a.read_sample;
        s.median_read_fraction = a.median_read_fraction;
        assert_eq!(score(&s), (Some(0), vec![]));
    }

    #[test]
    fn the_absolute_fallback_caps_at_eight() {
        let mut s = base(10, 10);
        s.dwell_sample = 3;
        s.median_dwell_ms = Some(0);
        let (score, reasons) = super::score(&s);
        assert_eq!(score, Some(8), "never 15 without a length");
        assert_eq!(reasons, vec![Reason::BriefLooks { median_dwell_ms: 0 }]);
        s.median_dwell_ms = Some(3_000);
        assert_eq!(super::score(&s).0, Some(0), "three seconds is not brief");
        s.median_dwell_ms = Some(2_999);
        s.dwell_sample = 2;
        assert_eq!(super::score(&s).0, Some(0), "and two looks are not a habit");
    }

    #[test]
    fn read_closely_subtracts_and_the_total_stops_at_zero() {
        let mut s = base(8, 0);
        s.dwell_sample = 3;
        s.read_sample = 3;
        s.median_read_fraction = Some(0.6);
        s.median_dwell_ms = Some(90_000);
        let (score, reasons) = super::score(&s);
        assert_eq!(score, Some(40), "55 - 15");
        assert_eq!(
            codes(&reasons),
            ["rarely_opened", "read_closely"],
            "the negative term sorts last"
        );
        s.opened = 8;
        assert_eq!(super::score(&s).0, Some(0), "clamped, not negative");
        s.median_read_fraction = Some(0.59);
        assert_eq!(
            super::score(&s).1,
            vec![],
            "just under the bar is not closely"
        );
    }

    #[test]
    fn skimmed_and_brief_looks_never_co_occur() {
        // Every combination of a tiny dwell with any read sample and fraction.
        for read_sample in [0, 2, 3, 9] {
            for fraction in [None, Some(0.0), Some(0.2), Some(0.5), Some(1.0)] {
                let mut s = base(10, 10);
                s.dwell_sample = 9;
                s.median_dwell_ms = Some(100);
                s.read_sample = read_sample;
                s.median_read_fraction = fraction;
                let codes = codes(&score(&s).1);
                let attention = codes
                    .iter()
                    .filter(|c| ["skimmed", "brief_looks", "read_closely"].contains(&c.as_str()))
                    .count();
                assert!(attention <= 1, "{read_sample} {fraction:?}: {codes:?}");
                // With a usable read fraction the absolute fallback is silent
                // even when the fraction itself earns nothing.
                if read_sample >= 3 && fraction.is_some() {
                    assert!(!codes.contains(&"brief_looks".to_string()), "{codes:?}");
                }
            }
        }
    }

    #[test]
    fn never_clicked_needs_five_opens_and_zero_clicks() {
        let mut s = base(10, 10);
        s.dwell_sample = 5;
        let (score, reasons) = super::score(&s);
        assert_eq!(score, Some(5));
        assert_eq!(reasons, vec![Reason::NeverClicked { opened: 5 }]);
        s.clicks = 1;
        assert_eq!(super::score(&s).0, Some(0));
        s.clicks = 0;
        s.dwell_sample = 4;
        assert_eq!(super::score(&s).0, Some(0));
    }

    #[test]
    fn volume_has_two_steps() {
        let mut s = base(10, 10);
        s.per_month = Some(7.9);
        assert_eq!(score(&s).0, Some(0));
        s.per_month = Some(8.0);
        assert_eq!(score(&s).0, Some(5));
        s.per_month = Some(20.0);
        let (score, reasons) = super::score(&s);
        assert_eq!(score, Some(10));
        assert_eq!(reasons, vec![Reason::HighVolume { per_month: 20.0 }]);
    }

    #[test]
    fn the_total_is_clamped_to_a_hundred() {
        // Every term at its ceiling is exactly 100; a store that over-counted
        // cleared mail must not push past it.
        let mut s = base(8, 0);
        s.cleared_unopened = 80;
        s.per_month = Some(30.0);
        assert_eq!(score(&s).0, Some(100));
    }

    #[test]
    fn reasons_are_ordered_by_contribution() {
        let mut s = base(20, 0);
        s.cleared_unopened = 20;
        s.dwell_sample = 5;
        s.read_sample = 5;
        s.median_read_fraction = Some(0.2);
        s.per_month = Some(25.0);
        let (score, reasons) = score(&s);
        assert_eq!(score, Some(93), "55 + 15 + 8 + 5 + 10");
        assert_eq!(
            codes(&reasons),
            [
                "rarely_opened",
                "cleared_unopened",
                "high_volume",
                "skimmed",
                "never_clicked"
            ]
        );
    }

    #[test]
    fn reasons_serialize_as_a_code_plus_numbers() {
        assert_eq!(
            serde_json::to_value(Reason::RarelyOpened {
                opened: 1,
                received: 23
            })
            .unwrap(),
            serde_json::json!({"code": "rarely_opened", "opened": 1, "received": 23})
        );
        assert_eq!(
            serde_json::to_value(Reason::NotEnoughHistory).unwrap(),
            serde_json::json!({"code": "not_enough_history"})
        );
    }

    #[test]
    fn read_fraction_is_dwell_over_expected_reading_time() {
        // 238 words take one minute.
        assert_eq!(read_fraction(238, 30_000), Some(0.5));
        assert_eq!(read_fraction(238, 600_000), Some(1.0), "clamped");
        assert_eq!(read_fraction(238, 0), Some(0.0));
        assert_eq!(read_fraction(39, 30_000), None, "too short to skim");
        assert!(read_fraction(40, 30_000).is_some());
    }

    #[test]
    fn attention_is_summarized_over_the_right_samples() {
        let sample = |dwell_ms, words, link_clicks| OpenSample {
            dwell_ms,
            link_clicks,
            words,
        };
        let a = summarize_attention(&[
            sample(60_000, 238, 1), // fraction 1.0
            sample(6_000, 238, 0),  // fraction 0.1
            sample(30_000, 476, 2), // fraction 0.25
            sample(1_000, 10, 0),   // no fraction
        ]);
        assert_eq!(a.dwell_sample, 4);
        assert_eq!(a.median_dwell_ms, Some(18_000), "mean of 6s and 30s");
        assert_eq!(a.clicks, 3);
        assert_eq!(a.read_sample, 3, "the ten-word mail has no fraction");
        assert_eq!(a.median_read_fraction, Some(0.25));
        assert_eq!(a.median_words, Some(238));
        assert_eq!(summarize_attention(&[]), AttentionSummary::default());
    }

    #[test]
    fn median_of_odd_even_and_empty() {
        assert_eq!(median(&mut []), None);
        assert_eq!(median(&mut [5]), Some(5));
        assert_eq!(median(&mut [9, 1, 5]), Some(5));
        assert_eq!(median(&mut [1, 4]), Some(2), "rounds down");
        assert_eq!(median(&mut [u64::MAX, u64::MAX]), Some(u64::MAX));
    }

    #[test]
    fn words_are_whitespace_separated() {
        assert_eq!(word_count("  one two\n\nthree\tfour "), 4);
        assert_eq!(word_count(""), 0);
    }
}
