//! The relationship ledger's arithmetic (docs/UNDERSTANDING.md §2.1): what
//! the owner's own history with one address adds up to.
//!
//! PURE: counts in, derived fields out. No store, no clock (the caller hands
//! in `now`), no network. The store module (`store::sqlite::relationships`)
//! does the counting with indexed probes on one address and hands the counts
//! here, so every rule about what a number MEANS lives in one place and is
//! unit-tested without a database.
//!
//! NOTHING HERE READS EMAIL TEXT. The inputs are counts and timestamps off
//! `messages`, `contacts`, `message_recipients`, `triage` and the correction
//! tables, plus the fatigue score, which reads word counts but never words.
//! So the ledger carries nothing a sender wrote and nothing in it is
//! untrusted, which is what lets the agent context carry it beside the
//! trusted `sender_is_contact` rather than beside the message body.
//!
//! WHAT IT IS NOT: a floor. Every consumer reads it as evidence about this
//! owner's behaviour. A reply rate of zero on bulk mail is the expected shape
//! and says nothing about a newsletter; a fatigue score is the Reading purge's
//! suggestion, not a verdict. Nothing here may reach `triage::ranking`.

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{Value, json};

use crate::triage::sender_fatigue::{self, MIN_COVERED_DAYS, Reason};

/// Below this many replied threads there is no median reply time. Two
/// answers are an anecdote: one written in a minute because the owner
/// happened to be at their desk says nothing about the next one.
pub const MIN_REPLIED_FOR_MEDIAN: usize = 3;

/// One inbound thread from this address: when the address first wrote into
/// it, and when the owner first wrote into it AFTER that, if ever.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThreadReply {
    pub first_inbound_at: DateTime<Utc>,
    pub reply_at: Option<DateTime<Utc>>,
}

/// Attention over the ledger floor, as the store counted it (see
/// [`Relationship::attention_rows`] for what the floor is).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct AttentionCounts {
    pub rows: u32,
    pub opened: u32,
    pub median_read_fraction: Option<f32>,
    pub link_clicks: u32,
}

/// Everything the store counted for one address, before any derivation.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Counts {
    pub received_count: i64,
    pub received_first_at: Option<DateTime<Utc>>,
    pub received_last_at: Option<DateTime<Utc>>,
    pub sent_count: i64,
    pub sent_last_at: Option<DateTime<Utc>>,
    pub initiated_count: i64,
    pub threads: Vec<ThreadReply>,
    pub attention: AttentionCounts,
    pub corrections: i64,
    /// `sender_fatigue::score` as the Reading purge computes it for this
    /// address (`reading_senders::SenderMeasurer`), never recomputed here.
    pub fatigue: (Option<u8>, Vec<Reason>),
}

/// One `relationships` row: the counts plus what they derive to.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Relationship {
    /// The lowercased bare address.
    pub addr: String,
    pub display_name: Option<String>,
    /// Inbound, not spam, not sealed, not the archive tier, not the owner's
    /// own mail: what the listings count.
    pub received_count: i64,
    /// Off `messages.received_at`, NEVER `senders.first_seen`: the backfill
    /// walks newest first, so the directory dates a ten-year correspondent to
    /// last Tuesday.
    pub received_first_at: Option<DateTime<Utc>>,
    pub received_last_at: Option<DateTime<Utc>>,
    /// See [`received_per_month`].
    pub received_per_month: Option<f32>,
    /// The Sent-derived `contacts` row: the known-contact floor's own count.
    pub sent_count: i64,
    pub sent_last_at: Option<DateTime<Utc>>,
    /// Threads the owner opened by writing first, to this address.
    pub initiated_count: i64,
    pub inbound_threads: i64,
    /// Of `inbound_threads`, the ones the owner wrote into later.
    pub replied_threads: i64,
    /// See [`median_reply_ms`].
    pub median_reply_ms: Option<i64>,
    /// The Reading purge's own counts for this address (`SenderSignals`):
    /// inbound mail received at or after the later of the 90-day window and
    /// the open ledger floor, and of those the opened ones. The read fraction
    /// and clicks are over the newest reported opens at or after the dwell
    /// ledger floor, the same sample the fatigue score reads.
    pub attention_rows: i64,
    pub opened_count: i64,
    pub median_read_fraction: Option<f32>,
    pub link_clicks: i64,
    /// `triage_feedback` plus `agent_triage_corrections` rows on this
    /// address's mail.
    pub corrections: i64,
    /// `sender_fatigue::score` on its 0..=1 scale (the purge flow's 0..=100,
    /// divided by a hundred), `None` when the module declined to score.
    pub fatigue_score: Option<f32>,
    pub fatigue_reasons: Vec<Reason>,
    pub computed_at: DateTime<Utc>,
}

/// The inbound monthly rate: `count` over the days since the address first
/// wrote, scaled to thirty days, to one decimal.
///
/// `None` under [`MIN_COVERED_DAYS`] of history with this address, which also
/// covers `sender_fatigue`'s week floor (`MIN_RATE_DAYS`): four mails in three
/// days is not "forty a month", and two weeks is the least the fatigue score
/// itself will read a pattern from. Measured from the address's own first
/// mail rather than the mailbox's, so a newsletter that started last month in
/// a mailbox holding a year does not read as a trickle.
pub fn received_per_month(
    count: i64,
    first_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Option<f32> {
    let first_at = first_at?;
    let days = (now - first_at).num_seconds() as f32 / 86_400.0;
    if count <= 0 || days < MIN_COVERED_DAYS {
        return None;
    }
    sender_fatigue::per_month(u32::try_from(count).unwrap_or(u32::MAX), days)
}

/// The median time from an address first writing into a thread to the
/// owner's first message after it, over the threads the owner answered.
/// `None` under [`MIN_REPLIED_FOR_MEDIAN`] of them.
///
/// A negative gap cannot come from the store (it asks for a LATER owner
/// message), but a clock-skewed header could write one; it is dropped rather
/// than allowed to pull the median under zero.
pub fn median_reply_ms(threads: &[ThreadReply]) -> Option<i64> {
    let mut gaps: Vec<u64> = threads
        .iter()
        .filter_map(|t| {
            t.reply_at
                .map(|at| (at - t.first_inbound_at).num_milliseconds())
        })
        .filter_map(|ms| u64::try_from(ms).ok())
        .collect();
    if gaps.len() < MIN_REPLIED_FOR_MEDIAN {
        return None;
    }
    sender_fatigue::median(&mut gaps).map(|ms| i64::try_from(ms).unwrap_or(i64::MAX))
}

/// Counts in, one row out.
pub fn derive(
    addr: &str,
    display_name: Option<String>,
    counts: Counts,
    now: DateTime<Utc>,
) -> Relationship {
    let replied = counts
        .threads
        .iter()
        .filter(|t| t.reply_at.is_some())
        .count();
    let (score, reasons) = counts.fatigue;
    Relationship {
        addr: addr.to_string(),
        display_name,
        received_count: counts.received_count,
        received_first_at: counts.received_first_at,
        received_last_at: counts.received_last_at,
        received_per_month: received_per_month(
            counts.received_count,
            counts.received_first_at,
            now,
        ),
        sent_count: counts.sent_count,
        sent_last_at: counts.sent_last_at,
        initiated_count: counts.initiated_count,
        inbound_threads: counts.threads.len() as i64,
        replied_threads: replied as i64,
        median_reply_ms: median_reply_ms(&counts.threads),
        attention_rows: i64::from(counts.attention.rows),
        opened_count: i64::from(counts.attention.opened.min(counts.attention.rows)),
        median_read_fraction: counts.attention.median_read_fraction,
        link_clicks: i64::from(counts.attention.link_clicks),
        corrections: counts.corrections,
        fatigue_score: score.map(|s| f32::from(s) / 100.0),
        fatigue_reasons: reasons,
        computed_at: now,
    }
}

/// Is there anything to keep a row for? An address with no live mail either
/// way (only archive mail, or mail that has since been deleted) has no
/// relationship the ledger can speak to, and a row of zeros would read as
/// "known, and ignored".
pub fn worth_a_row(counts: &Counts) -> bool {
    counts.received_count > 0 || counts.sent_count > 0 || counts.initiated_count > 0
}

/// A reply gap the way a person says it: "40m", "3h", "2d". The agent reads
/// this, and a millisecond count is a unit it would have to be told.
pub fn reply_gap(ms: i64) -> String {
    let minutes = (ms / 60_000).max(0);
    if minutes < 60 {
        format!("{}m", minutes.max(1))
    } else if minutes < 48 * 60 {
        format!("{}h", (minutes + 30) / 60)
    } else {
        format!("{}d", (minutes + 12 * 60) / (24 * 60))
    }
}

/// The block the triage agent reads, beside `sender_is_contact`
/// (docs/UNDERSTANDING.md §2.1, "What it feeds").
///
/// NULLS STAY NULLS: an unknown median is `null`, never zero, because zero is
/// a claim. THE ATTENTION BLOCK IS OMITTED ENTIRELY under
/// [`MIN_COVERED_DAYS`] of `coverage_days`: on a young ledger everything is
/// unopened, and a block of zeros would read as months of indifference.
pub fn agent_json(row: &Relationship, coverage_days: i64) -> Value {
    let rfc = |at: Option<DateTime<Utc>>| at.map(|at| at.to_rfc3339());
    let mut block = json!({
        "coverage_days": coverage_days,
        "received": {
            "count": row.received_count,
            "per_month": row.received_per_month,
            "last_at": rfc(row.received_last_at),
        },
        "sent": {
            "count": row.sent_count,
            "last_at": rfc(row.sent_last_at),
            "initiated": row.initiated_count,
        },
        "replies": {
            "threads": row.inbound_threads,
            "replied": row.replied_threads,
            "median_reply": row.median_reply_ms.map(reply_gap),
        },
        "corrections": row.corrections,
        "fatigue": {
            "score": row.fatigue_score,
            "reasons": row.fatigue_reasons,
        },
    });
    if (coverage_days as f32) >= MIN_COVERED_DAYS {
        block["attention"] = json!({
            "rows": row.attention_rows,
            "opened": row.opened_count,
            "median_read_fraction": row.median_read_fraction,
            "link_clicks": row.link_clicks,
        });
    }
    block
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone};

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 4, 12, 0, 0).unwrap()
    }

    fn thread(days_ago: i64, reply_after_mins: Option<i64>) -> ThreadReply {
        let first = now() - Duration::days(days_ago);
        ThreadReply {
            first_inbound_at: first,
            reply_at: reply_after_mins.map(|m| first + Duration::minutes(m)),
        }
    }

    #[test]
    fn the_monthly_rate_waits_for_two_weeks_of_history_with_the_address() {
        let ago = |days| Some(now() - Duration::days(days));
        assert_eq!(received_per_month(4, ago(3), now()), None, "three days");
        assert_eq!(received_per_month(4, ago(13), now()), None, "under 14");
        assert_eq!(received_per_month(30, ago(30), now()), Some(30.0));
        assert_eq!(received_per_month(6, ago(60), now()), Some(3.0));
        assert_eq!(received_per_month(0, ago(60), now()), None, "no mail");
        assert_eq!(received_per_month(5, None, now()), None, "no date");
    }

    #[test]
    fn the_median_reply_needs_three_answered_threads() {
        let two = [thread(10, Some(30)), thread(9, Some(90)), thread(8, None)];
        assert_eq!(median_reply_ms(&two), None);
        let three = [
            thread(10, Some(30)),
            thread(9, Some(90)),
            thread(8, Some(10)),
            thread(7, None),
        ];
        assert_eq!(median_reply_ms(&three), Some(30 * 60_000));
        // Even count: the middle pair averaged.
        let four = [
            thread(10, Some(10)),
            thread(9, Some(20)),
            thread(8, Some(40)),
            thread(7, Some(1_000)),
        ];
        assert_eq!(median_reply_ms(&four), Some(30 * 60_000));
    }

    #[test]
    fn a_skewed_header_cannot_pull_the_median_negative() {
        let threads = [
            thread(10, Some(-60)),
            thread(9, Some(5)),
            thread(8, Some(15)),
            thread(7, Some(25)),
        ];
        // The negative gap is dropped; three remain.
        assert_eq!(median_reply_ms(&threads), Some(15 * 60_000));
    }

    #[test]
    fn derive_counts_replied_threads_and_scales_fatigue() {
        let counts = Counts {
            received_count: 12,
            received_first_at: Some(now() - Duration::days(60)),
            received_last_at: Some(now() - Duration::days(1)),
            threads: vec![thread(50, Some(20)), thread(40, None), thread(30, Some(5))],
            attention: AttentionCounts {
                rows: 5,
                // A store that counted more opens than rows has a bug; the
                // row must not claim it.
                opened: 9,
                median_read_fraction: Some(0.4),
                link_clicks: 2,
            },
            fatigue: (Some(80), vec![Reason::NotEnoughHistory]),
            ..Default::default()
        };
        let row = derive("ann@fund.com", None, counts, now());
        assert_eq!(row.inbound_threads, 3);
        assert_eq!(row.replied_threads, 2);
        assert_eq!(row.median_reply_ms, None, "two replies is not a median");
        assert_eq!(row.received_per_month, Some(6.0));
        assert_eq!(row.opened_count, 5);
        assert_eq!(row.fatigue_score, Some(0.8));
        assert_eq!(row.computed_at, now());
    }

    #[test]
    fn a_row_needs_live_mail_one_way_or_the_other() {
        assert!(!worth_a_row(&Counts::default()));
        assert!(worth_a_row(&Counts {
            sent_count: 1,
            ..Default::default()
        }));
        assert!(worth_a_row(&Counts {
            received_count: 1,
            ..Default::default()
        }));
    }

    #[test]
    fn reply_gaps_read_like_a_person_says_them() {
        assert_eq!(reply_gap(10_000), "1m");
        assert_eq!(reply_gap(40 * 60_000), "40m");
        assert_eq!(reply_gap(3 * 3_600_000), "3h");
        assert_eq!(reply_gap(30 * 3_600_000), "30h");
        assert_eq!(reply_gap(3 * 86_400_000), "3d");
    }

    fn row() -> Relationship {
        derive(
            "news@letters.example",
            Some("Letters".into()),
            Counts {
                received_count: 212,
                received_first_at: Some(now() - Duration::days(200)),
                received_last_at: Some(now()),
                threads: vec![thread(100, None)],
                attention: AttentionCounts {
                    rows: 38,
                    opened: 2,
                    median_read_fraction: Some(0.1),
                    link_clicks: 0,
                },
                corrections: 1,
                fatigue: (
                    Some(80),
                    vec![Reason::RarelyOpened {
                        opened: 2,
                        received: 38,
                    }],
                ),
                ..Default::default()
            },
            now(),
        )
    }

    #[test]
    fn the_agent_block_has_the_designed_shape_and_keeps_its_nulls() {
        let block = agent_json(&row(), 41);
        assert_eq!(block["coverage_days"], 41);
        assert_eq!(block["received"]["count"], 212);
        assert!(block["received"]["per_month"].is_number());
        assert_eq!(
            block["sent"],
            json!({"count": 0, "last_at": null, "initiated": 0})
        );
        assert_eq!(
            block["replies"],
            json!({"threads": 1, "replied": 0, "median_reply": null})
        );
        assert_eq!(block["attention"]["rows"], 38);
        assert_eq!(block["attention"]["opened"], 2);
        assert_eq!(block["attention"]["link_clicks"], 0);
        assert_eq!(block["corrections"], 1);
        assert_eq!(
            block["fatigue"]["score"]
                .as_f64()
                .map(|s| (s * 100.0).round()),
            Some(80.0)
        );
        assert_eq!(block["fatigue"]["reasons"][0]["code"], "rarely_opened");
        // Nothing a sender wrote: no display name, no address.
        assert!(!block.to_string().contains("Letters"));
    }

    #[test]
    fn the_attention_block_is_absent_under_the_ledger_floor() {
        let young = agent_json(&row(), MIN_COVERED_DAYS as i64 - 1);
        assert!(young.get("attention").is_none(), "{young}");
        // Everything that does not depend on the dwell ledger is still there.
        assert_eq!(young["received"]["count"], 212);
        let ripe = agent_json(&row(), MIN_COVERED_DAYS as i64);
        assert!(ripe.get("attention").is_some());
    }
}
