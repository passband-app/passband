//! The senders behind the open mail in Reading, each with what the owner's
//! own behaviour says about them (`GET /client/v2/reading/senders`): the
//! evidence the purge flow shows when it suggests unsubscribing.
//!
//! HUMAN-DOOR ONLY, like `people` and `senders`: what somebody opens, how long
//! they look and whether they click is the most personal thing this store
//! holds, and the agent door was never handed it.
//!
//! READ-ONLY. Nothing here stamps `surfaced_at` or `opened_at` or writes
//! anything else: looking at the purge list is not reading the mail in it.
//!
//! ONE LOCK, BOUNDED WORK. Every store call shares one connection mutex with
//! sync, so the whole read is a single acquisition and each part of it is
//! capped:
//!
//! - WHICH senders is the Reading feed's own answer
//!   (`agent_triage::reading_items`), never a second membership query that
//!   could drift from what the feed shows.
//! - At most [`MAX_SENDERS`] of them are measured.
//! - Per sender, three seeks on `idx_messages_from_nocase` (pinned with
//!   `INDEXED BY`, see `rules::SENDER_RULE_DIGEST_SQL` for why), none of which
//!   reads a body.
//! - Bodies are reached by primary key, and only for the at most
//!   [`DWELL_SAMPLE_CAP`] opened messages per sender whose length the read
//!   fraction needs. Never for the window.
//!
//! WHAT IT COUNTS, and every exclusion is a decision:
//!
//! - NOT THE OWNER'S OWN MAIL, by authorship (`OWN_MAIL`), not bare
//!   `is_sent = 0`: a note-to-self or a send from another client is theirs and
//!   says nothing about a sender.
//! - NOT PROVIDER SPAM. Unopened by definition; it would make any sender whose
//!   mail Gmail sometimes junks look ignored.
//! - NOT SEALED MAIL (`sensitivity = 'normal'` only). A login code from a shop
//!   is not that shop's newsletter, and nothing about restricted mail leaves
//!   through a statistic.
//! - NEVER FURTHER BACK THAN THE OPEN LEDGER, for the reason
//!   `SqliteStore::share_open_rate` spells out: mail that arrived before opens
//!   were recorded divides a real denominator by an empty numerator.
//! - DWELL AND CLICKS ONLY FROM OPENS AT OR AFTER THE DWELL LEDGER FLOOR, the
//!   same argument one column over (`migrate::stamp_dwell_ledger_start`), and
//!   only from opens a client actually reported on (`SENDER_OPENS_SQL`).

use super::agent_triage::reading_items;
use super::attention::{dwell_ledger_since, open_ledger_since};
use super::messages::is_known_contact_conn;
use super::rules::list_sender_rules_conn;
use super::*;
use crate::store::agent_triage::AgentListQuery;
use crate::triage::rules::match_sender_rule;
use crate::triage::sender_fatigue::{
    self, OpenSample, Reason, SenderSignals, summarize_attention, word_count,
};

/// How far back a sender's statistics look, before the ledger floor shortens
/// it. Long enough that a weekly newsletter has a dozen issues to judge.
pub const STATS_WINDOW_DAYS: i64 = 90;

/// The most senders one read measures. The flow walks them one at a time, so
/// a hundred is more than a sitting, and it is what bounds the time this read
/// holds the store mutex.
pub const MAX_SENDERS: usize = 100;

/// How many of a sender's newest messages are offered as the place to
/// unsubscribe from. The newest is nearly always the one; the next two cover a
/// transactional mail that happened to arrive last and carries no link.
pub const UNSUB_CANDIDATES: usize = 3;

/// The most opened messages per sender that enter the dwell sample, newest
/// open first. A median settles long before this, and each one costs a body
/// read under the lock.
pub const DWELL_SAMPLE_CAP: usize = 25;

/// The shared universe of all three per-sender statements: one sender's
/// inbound, non-spam, non-sealed mail that is not the owner's own, received at
/// or after `?3`. A macro so the three cannot drift apart, and so static SQL
/// can `concat!` it.
macro_rules! sender_mail_sql {
    () => {
        concat!(
            " FROM messages m INDEXED BY idx_messages_from_nocase
              JOIN triage t ON t.message_id = m.id
              WHERE m.account_id = ?1
                AND m.from_addr = ?2 COLLATE NOCASE
                AND NOT ",
            own_mail_sql!(),
            " AND m.is_spam = 0
                AND t.sensitivity = 'normal'
                AND m.received_at >= ?3 "
        )
    };
}

/// Received, opened, and cleared without opening, for one sender.
pub(crate) const SENDER_COUNTS_SQL: &str = concat!(
    "SELECT COUNT(*), COUNT(t.opened_at),
            COALESCE(SUM(t.opened_at IS NULL AND t.status = 'done'), 0)",
    sender_mail_sql!()
);

/// One sender's opened mail that some client REPORTED ON, newest open first,
/// WITHOUT its body. The dwell floor is applied in Rust on the parsed stamp:
/// the rows come newest open first, so the floor only ever cuts a tail.
///
/// AN OPEN NOBODY REPORTED ON IS NOT A ZERO-SECOND LOOK. `/opened` and
/// `/attention` are separate calls, and a client from before the second
/// existed (or the phone, while only the Mac has shipped it) keeps stamping
/// opens with no dwell behind them long after the ledger floor. Counted, each
/// would read as a glance and a refusal to click, and a newsletter read
/// faithfully on the older client would rank as the first thing to purge. Any
/// client that reports at all reports more than zero for mail it showed, so
/// "something was reported" is what admits a row to the sample.
pub(crate) const SENDER_OPENS_SQL: &str = concat!(
    "SELECT m.id, t.opened_at, t.dwell_ms, t.link_clicks",
    sender_mail_sql!(),
    " AND t.opened_at IS NOT NULL
      AND (t.dwell_ms > 0 OR t.link_clicks > 0)
      ORDER BY t.opened_at DESC, m.id DESC
      LIMIT ?4"
);

/// One sender's newest messages with the header half of what the unsubscribe
/// classifier reads. No body: the handler fetches that, outside this lock,
/// only for a candidate whose header named no link.
pub(crate) const SENDER_UNSUB_CANDIDATES_SQL: &str = concat!(
    "SELECT m.id, m.list_unsubscribe, m.list_unsub_one_click, m.auth_pass",
    sender_mail_sql!(),
    " ORDER BY m.received_at DESC, m.id DESC
      LIMIT ?4"
);

/// The flattened plain text of one message, by primary key. `messages.body` is
/// what ingest feeds triage (`sync::ingest`: the plain part, else the HTML
/// through `html_to_text`), so a word count over it measures what a reader
/// reads and not the markup around it.
const MESSAGE_BODY_SQL: &str = "SELECT body FROM messages WHERE account_id = ?1 AND id = ?2";

/// A message a sender's unsubscribe link may be read from. The classification
/// itself lives in squelch-api and is not repeated here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsubCandidate {
    pub message_id: i64,
    pub list_unsubscribe: Option<String>,
    pub list_unsub_one_click: bool,
    pub auth_pass: Option<bool>,
}

/// The `unsubscribes` row for a sender the owner already asked to stop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SenderUnsubscribed {
    /// `unsubscribes.status`; `None` on a legacy browser row.
    pub status: Option<String>,
    pub requested_at: DateTime<Utc>,
}

/// One sender with open mail in Reading.
#[derive(Debug, Clone)]
pub struct ReadingSender {
    /// The lowercased bare address.
    pub sender: String,
    /// The name on their newest open Reading mail that carries one.
    pub display_name: Option<String>,
    /// Their open Reading messages right now, newest first.
    pub open_message_ids: Vec<i64>,
    pub signals: SenderSignals,
    /// The rule Stage-1 would apply to this address today.
    pub rule: Option<SenderRule>,
    pub unsub_candidates: Vec<UnsubCandidate>,
    pub unsubscribed: Option<SenderUnsubscribed>,
    pub score: Option<u8>,
    pub reasons: Vec<Reason>,
}

/// The whole answer.
#[derive(Debug, Clone)]
pub struct ReadingSenders {
    /// The window asked for, in days ([`STATS_WINDOW_DAYS`]).
    pub window_days: i64,
    /// The whole days the statistics actually cover: the window, shortened to
    /// the open ledger's age.
    pub ledger_days: i64,
    /// How many senders have open Reading mail, before [`MAX_SENDERS`].
    pub total_senders: usize,
    /// Score descending with unscored last, then volume, then address.
    pub senders: Vec<ReadingSender>,
}

impl SqliteStore {
    /// HUMAN-DOOR ONLY. See the module header for what is counted and why.
    ///
    /// `inventory` and `feed_limit` are the Reading feed's own arguments, so
    /// the senders here are exactly the senders of the items the feed returns
    /// for them.
    pub fn reading_senders(
        &self,
        account_id: AccountId,
        now: DateTime<Utc>,
        inventory: &AgentListQuery,
        feed_limit: usize,
    ) -> Result<ReadingSenders> {
        let conn = self.lock()?;

        // WHO: the feed's answer, grouped. Items arrive newest first, so each
        // group's ids are already newest first and the groups are in order of
        // their newest mail.
        let mut groups: Vec<(String, Option<String>, Vec<i64>)> = Vec::new();
        let mut index: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for item in reading_items(&conn, account_id, feed_limit, inventory, now)? {
            let sender = item.from_addr.trim().to_ascii_lowercase();
            if sender.is_empty() {
                continue;
            }
            let at = *index.entry(sender.clone()).or_insert_with(|| {
                groups.push((sender, None, Vec::new()));
                groups.len() - 1
            });
            let group = &mut groups[at];
            if group.1.is_none() {
                group.1 = item.from_name.map(|n| n.trim().to_string());
            }
            group.2.push(item.message_id);
        }
        let total_senders = groups.len();
        // WHICH HUNDRED, decided before anything is measured so the cap bounds
        // the work and not just the response: the senders with the most open
        // mail, since clearing Reading is what the flow is for. The sort is
        // stable, so ties stay in order of newest mail.
        groups.sort_by_key(|g| std::cmp::Reverse(g.2.len()));
        groups.truncate(MAX_SENDERS);

        // WHEN FROM. Whichever floor is later wins, as in `share_open_rate`.
        let window_floor = now - chrono::Duration::days(STATS_WINDOW_DAYS);
        let floor = window_floor.max(open_ledger_since(&conn, account_id)?);
        let dwell_floor = dwell_ledger_since(&conn, account_id)?;
        let covered = (now - floor).max(chrono::Duration::zero());
        // The ACTUAL length, never the nominal ninety: a week-old ledger that
        // saw seven mails saw thirty a month, not two. A day at least, so the
        // first hours of a ledger cannot scale one mail into hundreds.
        let covered_days = (covered.num_seconds() as f32 / 86_400.0).max(1.0);

        let rules = list_sender_rules_conn(&conn, account_id)?;
        let mut counts = conn.prepare(SENDER_COUNTS_SQL)?;
        let mut opens = conn.prepare(SENDER_OPENS_SQL)?;
        let mut candidates = conn.prepare(SENDER_UNSUB_CANDIDATES_SQL)?;
        let mut body = conn.prepare(MESSAGE_BODY_SQL)?;
        let mut unsubscribed = conn.prepare(
            "SELECT status, requested_at FROM unsubscribes
             WHERE account_id = ?1 AND sender_addr = ?2",
        )?;
        let floor_text = floor.to_rfc3339();

        let mut senders = Vec::with_capacity(groups.len());
        for (sender, display_name, open_message_ids) in groups {
            let (received, opened, cleared_unopened): (u32, u32, u32) = counts
                .query_row(params![account_id, sender, floor_text], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                })?;

            // The dwell sample: opened where dwell could be reported. A stamp
            // that will not parse is not evidence and is dropped, the same
            // direction every ledger failure falls.
            let opened_rows = opens
                .query_map(
                    params![account_id, sender, floor_text, DWELL_SAMPLE_CAP as i64],
                    |r| {
                        Ok((
                            r.get::<_, i64>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, i64>(2)?,
                            r.get::<_, i64>(3)?,
                        ))
                    },
                )?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            let mut samples = Vec::new();
            for (id, opened_at, dwell_ms, link_clicks) in opened_rows {
                let in_ledger = DateTime::parse_from_rfc3339(&opened_at)
                    .is_ok_and(|at| at.with_timezone(&Utc) >= dwell_floor);
                if !in_ledger {
                    continue;
                }
                // Borrowed straight from the row: the body is counted, never
                // copied out of SQLite.
                let words = body.query_row(params![account_id, id], |r| {
                    Ok(word_count(r.get_ref(0)?.as_str().unwrap_or("")))
                })?;
                samples.push(OpenSample {
                    dwell_ms: dwell_ms.max(0) as u64,
                    link_clicks: link_clicks.clamp(0, i64::from(u32::MAX)) as u32,
                    words,
                });
            }
            let attention = summarize_attention(&samples);

            let rule = match_sender_rule(&sender, &rules).cloned();
            let signals = SenderSignals {
                received,
                opened,
                cleared_unopened,
                dwell_sample: attention.dwell_sample,
                median_dwell_ms: attention.median_dwell_ms,
                clicks: attention.clicks,
                read_sample: attention.read_sample,
                median_read_fraction: attention.median_read_fraction,
                median_words: attention.median_words,
                per_month: (received as f32 * 30.0 / covered_days * 10.0).round() / 10.0,
                known_contact: is_known_contact_conn(&conn, account_id, &sender)?,
                surface_rule: rule
                    .as_ref()
                    .is_some_and(|r| r.disposition == Disposition::Surface),
            };

            // WHERE TO UNSUBSCRIBE FROM reads the ninety-day window and NOT
            // the ledger floor. The floor exists to keep a rate honest; a link
            // in a footer is not a rate, and on a young ledger the mail
            // sitting open in Reading is exactly the mail from before it.
            let unsub_candidates = candidates
                .query_map(
                    params![
                        account_id,
                        sender,
                        window_floor.to_rfc3339(),
                        UNSUB_CANDIDATES as i64
                    ],
                    |r| {
                        Ok(UnsubCandidate {
                            message_id: r.get(0)?,
                            list_unsubscribe: r.get(1)?,
                            list_unsub_one_click: r.get::<_, i64>(2)? != 0,
                            auth_pass: r.get::<_, Option<i64>>(3)?.map(|v| v != 0),
                        })
                    },
                )?
                .collect::<std::result::Result<Vec<_>, _>>()?;

            // Stored lowercased by every writer, so the UNIQUE index serves it.
            let unsubscribed = unsubscribed
                .query_row(params![account_id, sender], |r| {
                    Ok(SenderUnsubscribed {
                        status: r.get(0)?,
                        requested_at: dt(r, 1)?,
                    })
                })
                .optional()?;

            let (score, reasons) = sender_fatigue::score(&signals);
            senders.push(ReadingSender {
                sender,
                display_name: display_name.filter(|n| !n.is_empty()),
                open_message_ids,
                signals,
                rule,
                unsub_candidates,
                unsubscribed,
                score,
                reasons,
            });
        }

        // `Option<u8>` orders `None` below every `Some`, which is "unscored
        // last" once reversed.
        senders.sort_by(|a, b| {
            b.score
                .cmp(&a.score)
                .then_with(|| b.signals.per_month.total_cmp(&a.signals.per_month))
                .then_with(|| a.sender.cmp(&b.sender))
        });

        Ok(ReadingSenders {
            window_days: STATS_WINDOW_DAYS,
            ledger_days: covered.num_days().min(STATS_WINDOW_DAYS),
            total_senders,
            senders,
        })
    }
}
