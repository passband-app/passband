//! Keyword, semantic (vec0 KNN) and hybrid recall, plus the message-vector
//! writes that feed them. These are human/internal reads. External callers must
//! rehydrate results through `external_search_hits` before exposing any fields.

use super::*;
use crate::store::recency;
use rusqlite::params_from_iter;
use rusqlite::types::Value;
use std::collections::{HashMap, HashSet};
use zerocopy::AsBytes;

/// THE FTS5 MATCH WINDOW over the body column (column 1): up to 24 tokens
/// around the matched terms, `…` where the window jumps a gap. Internal markers
/// become plain text plus matched surface forms before leaving the store.
///
/// A column without matches yields its head. Keyword mapping uses the markers
/// to keep the stored snippet in that case; a sender-supplied control character
/// can fool this cosmetic check. Recall snippets establish body membership in
/// SQL before replacing the stored snippet.
const BODY_WINDOW: &str = "snippet(messages_fts, 1, char(1), char(2), '…', 24)";

/// [`BODY_WINDOW`] over the TRANSLATED body (column 3, `tbody`). Read only
/// when the original body has no window: a reader searching an English word
/// in French mail matched the translation, and the sentence that says it is
/// the English one. NULL for a message with no served translation.
const TBODY_WINDOW: &str = "snippet(messages_fts, 3, char(1), char(2), '…', 24)";

/// The stored snippet a human-door hit shows when no match window replaces
/// it: the translation's, when the message has a served one, else the
/// message's own. Over alias `m`. A macro so [`hit_columns_sql`] can
/// `concat!` it. The agent door never reads this: its hits are rehydrated
/// from `messages` alone in `external_search_hits`.
macro_rules! hit_snippet_sql {
    () => {
        "COALESCE((SELECT tr.snippet FROM message_translations tr
                    WHERE tr.message_id = m.id AND tr.status IN ('done','incomplete')
                      AND tr.snippet IS NOT NULL AND tr.snippet != ''), m.snippet)"
    };
}

/// THE RELEVANCE SCORE for every keyword-leg query: bm25 with the SUBJECT
/// weighted four times the body, and the translated subject and body
/// (`tsubject`, `tbody`) weighted exactly as the originals they translate.
///
/// A word in the subject is the sender saying what the mail is about; the same
/// word two hundred words into a newsletter is a coincidence. Unweighted bm25
/// cannot tell those apart — it only counts, and a long body can out-count a
/// three-word subject on term frequency alone. Four is a judgement, not a
/// measurement: enough that a subject match wins a near tie, small enough that
/// a body that is genuinely about the term still beats a subject that mentions
/// it once in passing.
///
/// Like `rank`, this is NEGATIVE and more negative is better, so `-bm25(...)`
/// is the relevance and every ORDER BY here reads biggest-first.
const BM25: &str = "bm25(messages_fts, 4.0, 1.0, 4.0, 1.0)";

/// The marker `BODY_WINDOW` plants on each matched term.
const SNIPPET_MARK: char = '\u{1}';

/// HOW FAR A DIAGNOSTIC COUNT COUNTS before it answers "that many, at least".
///
/// A thousand is far past every threshold anything reads (see
/// [`SqliteStore::fts_count`] for who reads them) and far short of a full scan
/// of a real mailbox. A capped count is reported as the cap, so a client can
/// tell "a thousand" from "more than we bothered to count" only by knowing this
/// number, which is the trade: the alternative is walking every doclist for a
/// term on every keystroke of an as-you-type search.
pub const DIAGNOSTIC_COUNT_CAP: u32 = 1_000;

// THE FTS TABLE DRIVES EVERY JOIN. Each query on this leg reads
// `FROM messages_fts f CROSS JOIN messages m ON m.id = f.rowid`, and the CROSS
// is load-bearing: it forbids the planner from reordering the pair. Left to
// itself, the planner sees `WHERE m.account_id = ?`, picks `idx_messages_from`
// as the OUTER loop and drives the MATCH as the inner one, which re-evaluates
// the FTS query once per candidate message instead of once. Measured on a copy
// of a 1,700-message mailbox: a single-term count 17 ms, a four-term OR count
// 62 ms, the strict/any seam count 97 ms, and with `partial=1` (the shape the
// panel sends on every debounced keystroke) 169-201 ms, all CPU, all under the
// store mutex. Pinned, every one of them returns the same rows in 0-1 ms.
// `the_fts_table_drives_every_join` in tests/search.rs holds the line.

/// The SQL behind [`SqliteStore::fts_count`]: params are `?1` account, `?2` the
/// MATCH expression, `?3` the cap. A free function so a test can `EXPLAIN` the
/// exact statement the store runs.
pub(super) fn fts_count_sql(include_sent: bool) -> String {
    let sent = if include_sent {
        ""
    } else {
        " AND m.is_sent = 0"
    };
    // The LIMIT sits INSIDE the counted subquery, which is what stops the
    // scan; a LIMIT on the COUNT itself would only limit the one row it
    // returns, after the whole doclist had been walked.
    format!(
        "SELECT COUNT(*) FROM (
             SELECT 1
             FROM messages_fts f
             CROSS JOIN messages m ON m.id = f.rowid
             LEFT JOIN triage t ON t.message_id = m.id
             WHERE m.account_id = ?1
               AND m.is_spam = 0{sent}
               AND messages_fts MATCH ?2
             LIMIT ?3
         )"
    )
}

/// Extract matched surface forms from the internal FTS highlight markers.
fn matched_words(raw: &str) -> Vec<String> {
    raw.split(SNIPPET_MARK)
        .skip(1)
        .filter_map(|part| {
            let (word, _) = part.split_once('\u{2}')?;
            (!word.is_empty()).then(|| word.to_string())
        })
        .collect()
}

fn body_window(raw: Option<String>) -> Option<String> {
    let raw = raw?;
    if raw.contains(SNIPPET_MARK) {
        Some(raw.replace([SNIPPET_MARK, '\u{2}'], ""))
    } else {
        None
    }
}

struct MatchExcerpt {
    snippet: Option<String>,
    snippet_matches: Vec<String>,
    subject_matches: Vec<String>,
}

impl MatchExcerpt {
    fn apply(self, hit: &mut SearchHit) {
        if let Some(snippet) = self.snippet {
            hit.snippet = snippet;
        }
        hit.snippet_matches = self.snippet_matches;
        hit.subject_matches = self.subject_matches;
    }
}

/// Everyone a message named, as one string over alias `m`: To, Cc and, ON THE
/// ACCOUNT'S OWN MAIL ONLY, Bcc. ONE EXPRESSION for the `to:` predicate in SQL
/// and the [`SearchHit::recipients`] the recall legs match it against
/// afterwards, so the two cannot disagree on who a message went to.
///
/// The Bcc gate is the reader's (`thread_view` in messages.rs): a SENT-walk
/// sighting of inbound mail can store the sender's leaked or forged `Bcc:`
/// before INBOX pins `is_sent` to 0, so the stored column alone is not "who
/// the owner blind-copied".
///
/// HUMAN DOOR ONLY in effect: recipients are human-door data (SECURITY.md
/// section 4), and both agent doors refuse `to:`.
macro_rules! recipients_sql {
    () => {
        concat!(
            "(COALESCE(m.to_addrs, '') || ', ' || COALESCE(m.cc_addrs, '') || ', ' || \
             CASE WHEN ",
            own_mail_sql!(),
            " THEN COALESCE(m.bcc_addrs, '') ELSE '' END)"
        )
    };
}

/// The envelope columns EVERY hit-producing SELECT starts with, in the order
/// [`map_search_hit`] reads them. A macro so a `prepare_cached` literal can
/// `concat!` it; one list, so no query hands back a hit whose `from_me` or
/// recipients were never selected.
///
/// AN ARCHIVE ROW READS AS DONE (column 7): see [`push_filter_clauses`].
///
/// The snippet column is the human door's ([`hit_snippet_sql`], a served
/// translation's when there is one) unless `original` is passed: the agent
/// door's hits carry the message's own snippet.
macro_rules! hit_columns_sql {
    () => {
        hit_columns_sql!(@snippet hit_snippet_sql!())
    };
    (original) => {
        hit_columns_sql!(@snippet "m.snippet")
    };
    (@snippet $snippet:expr) => {
        concat!(
            "m.id, m.thread_id, m.from_addr, m.from_name, m.subject, m.received_at, ",
            $snippet,
            ",
             (COALESCE(t.status, 'new') = 'done' OR m.is_archive = 1), ",
            own_mail_sql!(),
            ", ",
            recipients_sql!()
        )
    };
}
const HIT_COLUMNS: &str = hit_columns_sql!();

/// Columns [`HIT_COLUMNS`] fills, so a SELECT that appends its own can say
/// where they start.
const HIT_COLUMN_COUNT: usize = 10;

/// The envelope columns ([`HIT_COLUMNS`]), in SELECT order.
fn map_search_hit(r: &rusqlite::Row<'_>) -> rusqlite::Result<SearchHit> {
    Ok(SearchHit {
        id: r.get(0)?,
        thread_id: r.get(1)?,
        from_addr: r.get(2)?,
        from_name: r.get(3)?,
        subject: r.get(4)?,
        received_at: dt(r, 5)?,
        snippet: r.get(6)?,
        is_done: r.get(7)?,
        subject_matches: Vec::new(),
        snippet_matches: Vec::new(),
        from_me: r.get(8)?,
        recipients: r.get(9)?,
    })
}

/// Envelope, status, subject highlights, then the body match window and the
/// translated body's. Keep the stored snippet unless a returned window carries
/// a marker; the original body's window wins over the translation's.
fn map_search_hit_with_window(r: &rusqlite::Row<'_>) -> rusqlite::Result<SearchHit> {
    let mut hit = map_search_hit(r)?;
    hit.subject_matches = matched_words(&r.get::<_, String>(HIT_COLUMN_COUNT)?);
    // The original body's window, else the translated body's: an English
    // word in French mail matched the translation (see TBODY_WINDOW).
    for column in [HIT_COLUMN_COUNT + 1, HIT_COLUMN_COUNT + 2] {
        let raw: Option<String> = r.get(column)?;
        if let Some(window) = body_window(raw.clone()) {
            hit.snippet = window;
            hit.snippet_matches = raw.as_deref().map(matched_words).unwrap_or_default();
            break;
        }
    }
    Ok(hit)
}

/// Escape the LIKE metacharacters (`%`, `_`) and the escape character itself so
/// a user value is matched LITERALLY. Pairs with `ESCAPE '\'` in the SQL; every
/// `from:` predicate must carry both halves or a `%` in the reader's text turns
/// into a wildcard.
pub(super) fn escape_like(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 8);
    for c in value.chars() {
        if matches!(c, '\\' | '%' | '_') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Append the operator predicates ([`SearchFilter`]) to a WHERE clause under
/// construction, pushing each value onto `args` as a BOUND parameter — nothing
/// from the reader is ever interpolated into the SQL text.
///
/// Shared by the keyword path and the filter-only listing so the two cannot
/// drift apart in what `from:2026-01-01` means.
fn push_filter_clauses(sql: &mut String, args: &mut Vec<Value>, filter: &SearchFilter) {
    // AN ARCHIVE ROW READS AS DONE, here and in every hit this module builds
    // (the same expression, so the `done:` operator, the unfinished-first
    // grouping and a hit's own flag cannot disagree). Its triage row says
    // 'new' for good, because nothing ever triages it; taken at its word, a
    // year of old mail would sort ahead of everything the reader has finished.
    if let Some(done) = filter.done {
        sql.push_str(" AND (COALESCE(t.status, 'new') = 'done' OR m.is_archive = 1) = ?");
        args.push(Value::Integer(i64::from(done)));
    }
    if let Some(from) = &filter.from {
        // Substring on EITHER sender field: `from:jane` should find the address
        // and the display name. SQLite's LIKE is already ASCII-case-insensitive;
        // COLLATE NOCASE on the column operand says so out loud (and sits on the
        // column, not on the ESCAPE literal, where it would bind wrong).
        sql.push_str(
            " AND (m.from_addr COLLATE NOCASE LIKE ? ESCAPE '\\'
                   OR COALESCE(m.from_name, '') COLLATE NOCASE LIKE ? ESCAPE '\\')",
        );
        let pattern = format!("%{}%", escape_like(from));
        args.push(Value::Text(pattern.clone()));
        args.push(Value::Text(pattern));
    }
    if filter.from_me {
        // Authorship, not the stored `is_sent`: see `own_mail_sql!`. No bound
        // parameter, so it sits anywhere among the anonymous `?`s.
        sql.push_str(concat!(" AND ", own_mail_sql!()));
    }
    if let Some(to) = &filter.to {
        // Same reading as `from:`: a literal, case-insensitive substring.
        sql.push_str(concat!(
            " AND ",
            recipients_sql!(),
            " COLLATE NOCASE LIKE ? ESCAPE '\\'"
        ));
        args.push(Value::Text(format!("%{}%", escape_like(to))));
    }
    if let Some(after) = filter.after {
        // INCLUSIVE: 00:00:00 UTC of the named day is in range. Timestamps are
        // stored as RFC3339 UTC text, which sorts lexicographically.
        sql.push_str(" AND m.received_at >= ?");
        args.push(Value::Text(after.to_rfc3339()));
    }
    if let Some(before) = filter.before {
        // EXCLUSIVE: 00:00:00 UTC of the named day is out of range.
        sql.push_str(" AND m.received_at < ?");
        args.push(Value::Text(before.to_rfc3339()));
    }
}

/// One recall candidate as the leg that produced it saw it: the message id,
/// plus the `received_at` the recency blend needs.
///
/// The timestamp is carried OUT of the recall SQL rather than looked up
/// afterwards. Both legs already join `messages`, so it is free here — and the
/// blend has to happen BEFORE the top-`k` truncation, which is exactly when a
/// hydrated `SearchHit` does not exist yet.
#[derive(Clone, Copy)]
pub(super) struct Candidate {
    pub(super) id: i64,
    received_at: DateTime<Utc>,
}

/// The query vector for a HYBRID search, or `None` to run it keyword-only.
///
/// AN EMBED ERROR IS AN ABSENT EMBEDDER here, not a failed search. Locally an
/// embedder that built effectively never fails; the shared embedding service
/// fails on any network blip, a restart, or a full queue, and a search that
/// 500s over that is worse than one that answers from the keyword leg it
/// would have used before the embedder attached. `semantic_search` is the
/// exception and keeps propagating: it has nothing to degrade to.
///
/// The instruction is the QUERY side of BGE's asymmetry; the corpus vectors
/// were embedded without it, on purpose.
fn query_embed_or_degrade(embedder: &dyn crate::embed::Embedder, query: &str) -> Option<Vec<f32>> {
    match embedder.embed(&crate::embed::query_embed_text(query)) {
        Ok(qvec) => Some(qvec),
        Err(e) => {
            query_embed_degraded(&e);
            None
        }
    }
}

/// The one line a degraded hybrid search leaves. The error and nothing else:
/// never the query, which is the reader's words.
fn query_embed_degraded(e: &CoreError) {
    eprintln!("squelch: query embed failed ({e}); this search is keyword-only");
}

/// The RRF smoothing constant (the standard 60): how far down a list a hit can
/// sit before its vote stops mattering much.
const RRF_K: f32 = 60.0;

/// What one full recency vote is worth, as a fraction of being ranked FIRST on
/// one relevance list.
///
/// ON THESE LEGS RECENCY IS A TERM, NOT A FACTOR — the opposite of the keyword
/// leg's multiplication, and for the opposite reason. RRF scores are
/// deliberately FLAT at the head of a list (ranks 1 through 10 span 15% of one
/// list's vote), so a multiplicative boost with any useful range would stop
/// being a tilt and simply re-sort the top of the results by date. An additive
/// term denominated in the same `1 / (RRF_K + rank)` units stays comparable to
/// the thing it votes against: at 0.5, a fresh hit has to be within roughly a
/// dozen ranks on BOTH legs to overtake an ancient top hit.
const RRF_RECENCY_WEIGHT: f32 = 0.5;

/// The recency term added to one candidate's fused score.
fn recency_vote(received_at: DateTime<Utc>, now: DateTime<Utc>) -> f32 {
    RRF_RECENCY_WEIGHT * recency::boost(received_at, now) as f32 / (RRF_K + 1.0)
}

/// Fuse ranked candidate lists into one order, best first: Reciprocal Rank
/// Fusion across the lists a candidate appears in, plus its recency vote when
/// `sort` asks for one.
///
/// One list in is legal and useful — that is the semantic leg, where RRF is a
/// monotone restatement of the KNN order and the vote is the only thing that
/// can move a row. Under [`SearchSort::BestMatch`] that leg therefore returns
/// the KNN order untouched, which is the honest answer to "no time decay".
///
/// TIES BREAK by `received_at DESC, id DESC`, and that is not decoration. The
/// fused order is the sequence the door's cursor indexes into, while the score
/// map is a `HashMap` whose iteration order is not stable between calls — with
/// no explicit tiebreaker, equal scores could reshuffle between one page and
/// the next and drop or repeat rows across the boundary.
fn fuse_ranked(lists: &[&[Candidate]], sort: SearchSort, now: DateTime<Utc>) -> Vec<Candidate> {
    let mut score: HashMap<i64, f32> = HashMap::new();
    let mut seen: HashMap<i64, Candidate> = HashMap::new();
    for list in lists {
        for (rank, c) in list.iter().enumerate() {
            *score.entry(c.id).or_insert(0.0) += 1.0 / (RRF_K + rank as f32 + 1.0);
            seen.entry(c.id).or_insert(*c);
        }
    }
    let mut ranked: Vec<(Candidate, f32)> = score
        .into_iter()
        .map(|(id, s)| {
            let c = seen[&id];
            let vote = if sort.considers_recency() {
                recency_vote(c.received_at, now)
            } else {
                0.0
            };
            (c, s + vote)
        })
        .collect();
    ranked.sort_by(|a, b| {
        b.1.total_cmp(&a.1)
            .then(b.0.received_at.cmp(&a.0.received_at))
            .then(b.0.id.cmp(&a.0.id))
    });
    ranked.into_iter().map(|(c, _)| c).collect()
}

/// The widest recall window an external search may grow to, counted in rows
/// that passed the external prefilter (`external_candidate_sql!`): mail the
/// caller cannot be shown never takes a place in it. It bounds the per-row
/// authority checks one search can run under the store mutex.
pub const EXTERNAL_RECALL_MAX: usize = 4096;

/// WHO A RECALL LEG IS FETCHING FOR. `Internal` is the human door and the
/// daemon's own reads, and adds nothing to any statement: their SQL is
/// byte-for-byte what it was before this type existed. `External` appends the
/// prefilter to the leg's WHERE, so the rows a leg ranks, counts and cuts a
/// window out of are already the ones an external reader might be shown.
///
/// NEVER A PERMISSION. An `External` leg's output still goes through
/// [`SqliteStore::external_search_hits`] before any field of it is returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Audience {
    Internal,
    External,
}

impl Audience {
    /// The clause this audience adds to a WHERE over alias `m`. No bound
    /// parameters, so it can sit anywhere among the anonymous `?`s.
    fn clause(self) -> &'static str {
        match self {
            Self::Internal => "",
            Self::External => concat!(" AND ", external_candidate_sql!()),
        }
    }
}

/// Which legs an external search runs. The HTTP agent door lets its caller
/// choose; the MCP door is always [`Self::Hybrid`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalSearchMode {
    /// Keyword and vector, fused. Keyword alone while no embedder is attached.
    Hybrid,
    /// The vector leg alone. Errors without an embedder.
    Semantic,
    /// The keyword leg alone, operators applied in SQL; operators with no
    /// text are a newest-first listing.
    Keyword,
}

/// One recall leg of an external search: its ranked candidates (already
/// through the prefilter) and what the authority has said about each so far.
///
/// THE AUTHORITY IS APPLIED HERE, PER LEG, AND NOT ONLY TO THE FUSED PAGE. A
/// row the prefilter passes and the authority refuses (its decision rests on
/// a source that has since changed) would otherwise hold a rank on this list,
/// and rank is what fusion scores by.
struct ExternalLeg {
    ranked: Vec<Candidate>,
    /// How many rows `ranked` was fetched to hold; 0 before the first fetch.
    depth: usize,
    /// Whether a deeper fetch can return more (the keyword leg), or `ranked`
    /// is all there will be (the vector leg, fetched once at its ceiling).
    grows: bool,
    verdicts: HashMap<i64, bool>,
}

impl ExternalLeg {
    fn fixed(ranked: Vec<Candidate>) -> Self {
        Self {
            depth: ranked.len(),
            ranked,
            grows: false,
            verdicts: HashMap::new(),
        }
    }

    fn growing() -> Self {
        Self {
            ranked: Vec::new(),
            depth: 0,
            grows: true,
            verdicts: HashMap::new(),
        }
    }
}

impl SqliteStore {
    /// Rehydrate externally returned hits under the same lock as current access
    /// validation. Search indexes may contain old/restricted content; their
    /// cached snippets and fields are never themselves permission to disclose it.
    /// Preserve the retrieval order after removing unavailable candidates.
    ///
    /// THIS IS THE AUTHORITY for every external search result: the full
    /// per-message check (its own assessment, the owner's seal, and every
    /// source its decision consumed) and the thread guard, asked as one walk
    /// (`messages::thread_allowed_with_conn`). Whatever a recall leg filtered
    /// beforehand, nothing is returned that fails here.
    pub fn external_search_hits(
        &self,
        account_id: AccountId,
        hits: &[SearchHit],
    ) -> Result<Vec<SearchHit>> {
        let conn = self.lock()?;
        let mut allowed = Vec::new();
        for id in hits.iter().map(|hit| hit.id) {
            let hit = conn
                .prepare_cached(concat!(
                    "SELECT ",
                    hit_columns_sql!(original),
                    " FROM messages m LEFT JOIN triage t ON t.message_id=m.id
                     WHERE m.account_id=?1 AND m.id=?2 AND m.is_spam=0"
                ))?
                .query_row(params![account_id, id], map_search_hit)
                .optional()?;
            let Some(hit) = hit else {
                continue;
            };
            // The hit itself by name, and its thread's guard, in one walk.
            if super::messages::thread_allowed_with_conn(&conn, account_id, &hit.thread_id, &[id])?
            {
                allowed.push(hit);
            }
        }
        Ok(allowed)
    }

    /// ASSESS ON SURFACING, the choosing half: the unassessed archive rows the
    /// KEYWORD leg matches for `text` that a check can still be bought for
    /// (`archive_askable_sql!`), best match first, at most
    /// [`ARCHIVE_SURFACE_CAP`](crate::store::ARCHIVE_SURFACE_CAP) of them.
    /// Strict matches before any-term ones, exactly as the leg itself ranks.
    ///
    /// ITS OWN QUERY, and a bounded one. These rows are hidden from the
    /// search proper (see [`Audience`]), so they cannot be read off its
    /// window; and nothing wider than what one call may queue is ever
    /// collected, so no caller's result or latency grows with how much
    /// unreadable mail a word matches.
    ///
    /// A KEYWORD MATCH OR NOTHING. No text an index can look up (operators
    /// alone, punctuation) is an empty list, whatever the filter says: a date
    /// range or a sender is not a match, and must not buy checks for whatever
    /// mail falls inside it. The vector leg never contributes for the same
    /// reason: nearest neighbours are not matches.
    ///
    /// Crate-private on purpose: an id here is a row the caller has not been
    /// allowed to see, and neither door can read, count or branch on it. Its
    /// one consumer is `store::archive_surface`, which queues the checks.
    pub(crate) fn archive_surface_candidates(
        &self,
        account_id: AccountId,
        text: &str,
        filter: &SearchFilter,
        include_sent: bool,
    ) -> Result<Vec<i64>> {
        let fts = FtsQuery::build(text, false);
        if fts.is_empty() {
            return Ok(Vec::new());
        }
        let cap = crate::store::ARCHIVE_SURFACE_CAP;
        let conn = self.lock()?;
        let pass = |expr: &str, exclude: Option<&str>, limit: usize| -> Vec<i64> {
            let mut sql = String::from(concat!(
                "SELECT m.id
                 FROM messages_fts f
                 CROSS JOIN messages m ON m.id = f.rowid
                 LEFT JOIN triage t ON t.message_id = m.id
                 LEFT JOIN agent_message_state a
                   ON a.account_id = m.account_id AND a.message_id = m.id
                 WHERE m.account_id = ?
                   AND messages_fts MATCH ?
                   AND ",
                archive_askable_sql!()
            ));
            let mut args = vec![Value::Integer(account_id), Value::Text(expr.to_string())];
            if let Some(exclude) = exclude {
                sql.push_str(
                    " AND f.rowid NOT IN (
                         SELECT rowid FROM messages_fts WHERE messages_fts MATCH ?)",
                );
                args.push(Value::Text(exclude.to_string()));
            }
            if !include_sent && !filter.searches_sent() {
                sql.push_str(" AND m.is_sent = 0");
            }
            push_filter_clauses(&mut sql, &mut args, filter);
            sql.push_str(&format!(
                " ORDER BY {BM25}, m.received_at DESC, m.id DESC LIMIT ?"
            ));
            args.push(Value::Integer(limit as i64));
            // Same reading as every other MATCH on this leg: a query the
            // index cannot run matched nothing.
            let Ok(mut stmt) = conn.prepare(&sql) else {
                return Vec::new();
            };
            let Ok(rows) = stmt.query_map(params_from_iter(args), |r| r.get::<_, i64>(0)) else {
                return Vec::new();
            };
            rows.map_while(std::result::Result::ok).collect()
        };
        let mut out = pass(&fts.strict, None, cap);
        if fts.terms.len() > 1 && out.len() < cap {
            out.extend(pass(&fts.any, Some(&fts.strict), cap - out.len()));
        }
        Ok(out)
    }

    /// The MCP door's search: [`Self::external_search_filtered`] in hybrid
    /// mode with no operators.
    pub fn external_search(
        &self,
        account_id: AccountId,
        query: &str,
        sort: SearchSort,
        limit: usize,
    ) -> Result<Vec<SearchHit>> {
        self.external_search_filtered(
            account_id,
            query,
            &SearchFilter::default(),
            sort,
            ExternalSearchMode::Hybrid,
            limit,
        )
    }

    /// THE ONE SEARCH BOTH AGENT DOORS RUN. Up to `limit` hits an external
    /// reader may be shown, in rank order.
    ///
    /// HIDDEN MAIL NEVER SHAPES THE ANSWER. Two steps keep it out, and the
    /// second is the one that decides anything:
    /// 1. each leg fetches as [`Audience::External`], so the rows it ranks
    ///    already pass the prefilter (`external_candidate_sql!`);
    /// 2. each leg's list is then cut down to the rows the AUTHORITY allows
    ///    ([`ExternalLeg`]) BEFORE the legs are fused. Fusion scores a row by
    ///    its rank on each list, so one unreadable row left on a list would
    ///    shift every readable row beneath it and reorder the page.
    ///
    /// What is fused, windowed and paged is therefore readable mail only, and
    /// the hits are rehydrated through [`Self::external_search_hits`] on the
    /// way out, which asks the authority once more.
    ///
    /// THE QUERY IS EMBEDDED ONCE and the vector leg is asked once
    /// ([`Self::nearest_readable`]), for the [`EXTERNAL_RECALL_MAX`] nearest
    /// rows AMONG THOSE THAT PASS THE PREFILTER; each wider window is a longer
    /// prefix of that same list. It is not the human door's KNN, which picks
    /// its neighbours over the whole account and would let mail the caller
    /// cannot read fill the list.
    pub fn external_search_filtered(
        &self,
        account_id: AccountId,
        text: &str,
        filter: &SearchFilter,
        sort: SearchSort,
        mode: ExternalSearchMode,
        limit: usize,
    ) -> Result<Vec<SearchHit>> {
        let knn = |required: bool| -> Result<ExternalLeg> {
            let Some(embedder) = self.embedder() else {
                if required {
                    return Err(CoreError::InvalidInput("no embedder attached".into()));
                }
                return Ok(ExternalLeg::fixed(Vec::new()));
            };
            let qvec = match embedder.embed(&crate::embed::query_embed_text(text)) {
                Ok(qvec) => qvec,
                // Hybrid degrades the way it does with no embedder; the vector
                // leg alone has nothing to fall back to. See
                // `query_embed_or_degrade`.
                Err(e) if !required => {
                    query_embed_degraded(&e);
                    return Ok(ExternalLeg::fixed(Vec::new()));
                }
                Err(e) => return Err(e),
            };
            Ok(ExternalLeg::fixed(self.nearest_readable(
                account_id,
                &qvec,
                EXTERNAL_RECALL_MAX,
            )?))
        };
        match mode {
            ExternalSearchMode::Hybrid => {
                let mut neighbours = knn(false)?;
                let fts = FtsQuery::build(text, false);
                let mut keyword = ExternalLeg::growing();
                self.external_recall(account_id, limit, |window| {
                    let vec_hits =
                        self.external_leg(account_id, &mut neighbours, window, |_| Ok(Vec::new()))?;
                    let fts_hits = self.external_leg(account_id, &mut keyword, window, |n| {
                        self.fts_recall(account_id, &fts, n, Audience::External)
                    })?;
                    // No snippet windows: every hit is rehydrated by the loop.
                    let (legs, full) = self.hybrid_fuse(
                        account_id,
                        &fts,
                        vec_hits,
                        fts_hits,
                        filter,
                        sort,
                        0..0,
                        window,
                    )?;
                    Ok((legs.into_iter().map(|leg| leg.hit).collect(), full))
                })
            }
            ExternalSearchMode::Semantic => {
                let mut neighbours = knn(true)?;
                self.external_recall(account_id, limit, |window| {
                    let candidates =
                        self.external_leg(account_id, &mut neighbours, window, |_| Ok(Vec::new()))?;
                    let full = candidates.len() == window;
                    let hits =
                        self.semantic_hits_from(account_id, &candidates, None, filter, sort)?;
                    Ok((hits, full))
                })
            }
            // ONE RANKED LIST and nothing fused, so a row the authority
            // refuses costs a wider window and never a different order.
            ExternalSearchMode::Keyword => self.external_recall(account_id, limit, |window| {
                let conn = self.lock()?;
                let (hits, _) = self.search_filtered_locked(
                    &conn,
                    account_id,
                    text,
                    filter,
                    sort,
                    false,
                    window as u32,
                    0,
                    Audience::External,
                )?;
                let full = hits.len() == window;
                Ok((hits, full))
            }),
        }
    }

    /// The best `want` READABLE candidates of one external leg, in the leg's
    /// own order: walk its ranked list, ask the authority about each row
    /// (once; the answers are kept for the rest of this search), stop when
    /// `want` have passed. A growing leg that runs out of fetched rows while
    /// the index may hold more is fetched again, twice as deep, up to
    /// [`EXTERNAL_RECALL_MAX`] rows; `fetch(n)` returns its best `n`.
    fn external_leg(
        &self,
        account_id: AccountId,
        leg: &mut ExternalLeg,
        want: usize,
        mut fetch: impl FnMut(usize) -> Result<Vec<Candidate>>,
    ) -> Result<Vec<Candidate>> {
        loop {
            if leg.depth == 0 && leg.grows {
                leg.depth = want.clamp(1, EXTERNAL_RECALL_MAX);
                leg.ranked = fetch(leg.depth)?;
            }
            let mut readable = Vec::with_capacity(want.min(leg.ranked.len()));
            {
                let conn = self.lock()?;
                for candidate in &leg.ranked {
                    if readable.len() >= want {
                        break;
                    }
                    let allowed = match leg.verdicts.get(&candidate.id) {
                        Some(allowed) => *allowed,
                        None => {
                            let allowed = super::messages::external_contributor_allowed_conn(
                                &conn,
                                account_id,
                                candidate.id,
                            )?;
                            leg.verdicts.insert(candidate.id, allowed);
                            allowed
                        }
                    };
                    if allowed {
                        readable.push(*candidate);
                    }
                }
            }
            let may_hold_more =
                leg.grows && leg.ranked.len() == leg.depth && leg.depth < EXTERNAL_RECALL_MAX;
            if readable.len() >= want || !may_hold_more {
                return Ok(readable);
            }
            leg.depth = leg.depth.saturating_mul(2).min(EXTERNAL_RECALL_MAX);
            leg.ranked = fetch(leg.depth)?;
        }
    }

    /// THE ONE RECALL LOOP every external search runs. `fetch(window)` returns
    /// the best `window` candidates and whether that window was full; the
    /// authority is applied to them, and the window doubles until `limit`
    /// readable hits survive, the corpus is exhausted, or it reaches
    /// [`EXTERNAL_RECALL_MAX`]. With legs that hand over readable rows only,
    /// what makes a window come up short is the caller's own operators
    /// (`from:`, a date), applied after the ranking.
    fn external_recall(
        &self,
        account_id: AccountId,
        limit: usize,
        fetch: impl FnMut(usize) -> Result<(Vec<SearchHit>, bool)>,
    ) -> Result<Vec<SearchHit>> {
        self.external_recall_within(account_id, limit, EXTERNAL_RECALL_MAX, fetch)
    }

    /// [`Self::external_recall`] with the ceiling as a parameter, so a test
    /// can reach it without four thousand rows.
    pub(crate) fn external_recall_within(
        &self,
        account_id: AccountId,
        limit: usize,
        max_window: usize,
        mut fetch: impl FnMut(usize) -> Result<(Vec<SearchHit>, bool)>,
    ) -> Result<Vec<SearchHit>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let mut window = limit.min(max_window);
        loop {
            let (candidates, full) = fetch(window)?;
            let mut found = self.external_search_hits(account_id, &candidates)?;
            if found.len() >= limit || !full || window >= max_window {
                found.truncate(limit);
                return Ok(found);
            }
            window = window.saturating_mul(2).min(max_window);
        }
    }

    /// The date search reaches back to when there is no archive walk to ask:
    /// the oldest `received_at` date that has at least three non-spam,
    /// agent-readable messages in the seven days from it. ONE ROW NEVER SETS
    /// IT. `received_at` prefers the `Date` header, so a single message
    /// claiming 2011 would otherwise make the horizon promise years of
    /// coverage, and a horizon read off one row's timestamp would hand that
    /// row's date to whoever asked. Dates before 1990 are broken headers.
    ///
    /// ONLY MAIL ASSESSED AS ALLOWED COUNTS, because this date is reported at
    /// the agent door: built from every row, it would move with the arrival
    /// dates of mail that door may not read. It is the horizon of what an
    /// external caller can find, which is the question it was asked.
    pub fn indexed_since(&self, account_id: AccountId) -> Result<Option<chrono::NaiveDate>> {
        let conn = self.lock()?;
        let oldest: Option<String> = conn
            .query_row(
                "SELECT date(m.received_at) FROM messages m
                 JOIN agent_message_state a ON a.account_id=m.account_id AND a.message_id=m.id
                 WHERE m.account_id=?1 AND m.is_spam=0 AND m.received_at>='1990-01-01'
                   AND a.access='allowed'
                   AND (SELECT COUNT(*) FROM (SELECT 1 FROM messages n
                        JOIN agent_message_state b
                          ON b.account_id=n.account_id AND b.message_id=n.id
                        WHERE n.account_id=m.account_id AND n.is_spam=0
                          AND b.access='allowed'
                          AND n.received_at>=m.received_at
                          AND n.received_at<strftime('%Y-%m-%dT%H:%M:%S',m.received_at,'+7 days')
                        LIMIT 3))>=3
                 ORDER BY m.received_at ASC LIMIT 1",
                params![account_id],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        Ok(oldest.and_then(|date| chrono::NaiveDate::parse_from_str(&date, "%Y-%m-%d").ok()))
    }

    // ON-BOX SEMANTIC RECALL. Inherent methods rather than `Store` ones because
    // they need the attached [`Embedder`] and the sqlite-vec `message_vecs`
    // table, which not every `Store` impl carries.
    //
    // Internal embedding and human recall may process restricted mail. External
    // results always pass through current source checks in external_search_hits.

    /// SEMANTIC RECALL: embed `query_text` and return the `k` nearest messages as
    /// `(message_id, distance)`, smaller = closer, scoped to `account_id`.
    ///
    /// Internal recall includes restricted mail and the user's own sent mail.
    /// This primitive does not authorize exposing a result to an external agent.
    ///
    /// RAW KNN: this is nearest-by-meaning and nothing else. Recency belongs to
    /// the SEARCH surfaces built on top of it — see
    /// [`semantic_search_hits`](Self::semantic_search_hits) — not to the
    /// primitive they share.
    pub fn semantic_search(
        &self,
        account_id: AccountId,
        query_text: &str,
        k: usize,
    ) -> Result<Vec<(i64, f32)>> {
        Ok(self
            .semantic_knn(account_id, query_text, k)?
            .into_iter()
            .map(|(c, dist)| (c.id, dist))
            .collect())
    }

    /// Embed `query_text` and KNN it: the shared body of [`semantic_search`] and
    /// [`semantic_search_hits`](Self::semantic_search_hits). Errors when no
    /// embedder is attached, which is what makes `mode=semantic` a hard failure
    /// before the background attach rather than a silently empty result.
    fn semantic_knn(
        &self,
        account_id: AccountId,
        query_text: &str,
        k: usize,
    ) -> Result<Vec<(Candidate, f32)>> {
        let embedder = self
            .embedder()
            .ok_or_else(|| CoreError::InvalidInput("no embedder attached".into()))?;
        let qvec = embedder.embed(&crate::embed::query_embed_text(query_text))?;
        self.knn_by_vector(account_id, &qvec, k)
    }

    /// Lower-level KNN used by [`semantic_knn`](Self::semantic_knn) (and reused
    /// by [`hybrid_search`]): given an already-computed query vector, return the
    /// `k` nearest account-owned messages for the account, each with its distance.
    ///
    /// THE HUMAN DOOR AND THE DAEMON ONLY. vec0 picks its `k` over the whole
    /// account and the joins filter afterwards, so this cannot be narrowed to
    /// what an external caller may read; that caller's vector leg is
    /// [`Self::nearest_readable`].
    fn knn_by_vector(
        &self,
        account_id: AccountId,
        query: &[f32],
        k: usize,
    ) -> Result<Vec<(Candidate, f32)>> {
        if query.len() != VEC_DIMS {
            return Err(CoreError::InvalidInput(format!(
                "query embedding len {} != vec0 width {VEC_DIMS}",
                query.len()
            )));
        }
        let conn = self.lock()?;
        // vec0 KNN: MATCH the embedding, constrain by the account_id metadata
        // column, cap with `k = ?`, then re-join triage to drop any sealed row
        // that should never have been indexed in the first place.
        //
        // TWICE, once per vector table: the original's and the English
        // translation's (docs/TRANSLATION.md). Each gives its own `k` nearest
        // over the whole account, and a message is as near as the NEARER of
        // its two vectors, so the merge keeps the smaller distance per message
        // before cutting back to `k`. Every filter is the same on both.
        let mut best: HashMap<i64, (Candidate, f32)> = HashMap::new();
        for table in ["message_vecs", "message_translation_vecs"] {
            let mut stmt = conn.prepare_cached(&format!(
                "SELECT v.message_id, v.distance, m.received_at
                 FROM {table} v
                 JOIN messages m ON m.id = v.message_id
                 LEFT JOIN triage t ON t.message_id = v.message_id
                 WHERE v.embedding MATCH ?1
                   AND v.account_id = ?2
                   AND v.k = ?3
                   AND m.is_spam = 0
                 ORDER BY v.distance"
            ))?;
            let rows = stmt.query_map(params![query.as_bytes(), account_id, k as i64], |r| {
                Ok((
                    Candidate {
                        id: r.get::<_, i64>(0)?,
                        received_at: dt(r, 2)?,
                    },
                    r.get::<_, f64>(1)? as f32,
                ))
            })?;
            for row in rows {
                let (c, dist) = row?;
                best.entry(c.id)
                    .and_modify(|e| {
                        if dist < e.1 {
                            e.1 = dist;
                        }
                    })
                    .or_insert((c, dist));
            }
        }
        let mut out: Vec<(Candidate, f32)> = best.into_values().collect();
        // Distance, then id: a stable order for equal distances, as vec0's
        // own single-table order was for the one list it returned.
        out.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.0.id.cmp(&b.0.id)));
        out.truncate(k);
        Ok(out)
    }

    /// THE EXTERNAL VECTOR LEG: the `k` nearest messages AMONG THOSE THAT PASS
    /// THE PREFILTER (`external_candidate_sql!`), nearest first. An exact
    /// scan, not vec0's KNN: the readable rows are chosen first, each one's
    /// stored vector is read by its key, and the distance is computed in SQL.
    /// A row the caller cannot be shown is never measured, so no number of
    /// them, however near, moves a readable row down this list or off it.
    ///
    /// `vec_distance_l2` because `message_vecs` declares no `distance_metric`
    /// and L2 is vec0's default: the same number [`Self::knn_by_vector`]
    /// orders by, so the two doors rank the same rows the same way.
    ///
    /// STILL ONLY A PREFILTER. Every row returned goes through the authority
    /// ([`ExternalLeg`], then [`Self::external_search_hits`]) like any other.
    ///
    /// COST: one vector read and one 384-wide distance per readable row of
    /// the account, with no index, plus one pass over the account's messages
    /// to build the set of closed threads (the flat form of the prefilter;
    /// the correlated form re-scans a row's siblings for every row, which on
    /// hundred-message threads took this leg from 0.3 s to 2 s at 10,000
    /// rows). All of it under the store lock.
    /// `the_external_vector_leg_over_ten_thousand_readable_rows` in
    /// tests/archive.rs times it.
    pub(super) fn nearest_readable(
        &self,
        account_id: AccountId,
        query: &[f32],
        k: usize,
    ) -> Result<Vec<Candidate>> {
        if query.len() != VEC_DIMS {
            return Err(CoreError::InvalidInput(format!(
                "query embedding len {} != vec0 width {VEC_DIMS}",
                query.len()
            )));
        }
        let conn = self.lock()?;
        // CROSS JOIN pins the order: readable rows first, then a point read of
        // each one's vector. `+v.account_id` keeps that test out of vec0's
        // planner, which would otherwise be offered it as a KNN constraint.
        // Ties break on the id so the list is the same list every time.
        //
        // THE TRANSLATION'S VECTOR is a second point read of the SAME
        // readable row, never a second candidate set: it is LEFT JOINed onto
        // a row the prefilter already passed, so it reaches exactly the rows
        // the original's vector does, and the row ranks by the nearer of the
        // two. Its account test sits in the ON clause, where a failing match
        // leaves the original's distance alone rather than dropping the row.
        let mut stmt = conn.prepare_cached(concat!(
            "SELECT m.id, m.received_at
             FROM messages m
             CROSS JOIN message_vecs v ON v.message_id = m.id
             LEFT JOIN message_translation_vecs tv
               ON tv.message_id = m.id AND +tv.account_id = ?2
             WHERE m.account_id = ?2
               AND m.is_spam = 0
               AND +v.account_id = ?2
               AND ",
            external_candidate_flat_sql!("?2"),
            " ORDER BY CASE WHEN tv.message_id IS NULL
                       THEN vec_distance_l2(v.embedding, ?1)
                       ELSE MIN(vec_distance_l2(v.embedding, ?1),
                                vec_distance_l2(tv.embedding, ?1)) END, m.id
             LIMIT ?3"
        ))?;
        let rows = stmt.query_map(params![query.as_bytes(), account_id, k as i64], |r| {
            Ok(Candidate {
                id: r.get::<_, i64>(0)?,
                received_at: dt(r, 1)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// The ids the flat prefilter passes, for the test that holds it equal to
    /// the correlated one.
    #[cfg(test)]
    pub(crate) fn external_flat_prefilter_ids(&self, account_id: AccountId) -> Result<Vec<i64>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(concat!(
            "SELECT m.id FROM messages m WHERE m.account_id = ?2 AND ?1 IS NULL AND ",
            external_candidate_flat_sql!("?2")
        ))?;
        let ids = stmt
            .query_map(params![Option::<i64>::None, account_id], |r| r.get(0))?
            .collect::<std::result::Result<Vec<i64>, _>>()?;
        Ok(ids)
    }

    /// HYBRID RECALL: merge FTS5 keyword rank and vector distance with Reciprocal
    /// Rank Fusion — each candidate scores `sum(1 / (rrf_k + rank))` across the
    /// lists it appears in, `rrf_k` being the standard smoothing constant (60),
    /// plus a RECENCY vote (see [`fuse_ranked`]). Keyword catches exact tokens,
    /// vectors catch paraphrase, recency breaks the near-ties those two leave
    /// behind, and any one of them alone still produces results. Both recall
    /// legs exclude sealed rows and include sent mail (recall).
    ///
    /// The recency vote is applied BEFORE the top-`k` truncation, so it decides
    /// what makes the window rather than just how the window is displayed.
    ///
    /// `filter` is applied POST-HOC to the hydrated top-`k` window, because
    /// neither recall leg can express `from:`/date bounds in its ranking. See
    /// [`semantic_search_hits`](Self::semantic_search_hits) for what that costs.
    ///
    /// The second return value is WINDOW FULL: the fused candidate pool reached
    /// `k`, so mail below the window may exist. The door's pagination needs it —
    /// a filter can shrink a full window to a short page, and "short page"
    /// alone would read as "no more results" when there may be plenty.
    pub fn hybrid_search(
        &self,
        account_id: AccountId,
        query_text: &str,
        filter: &SearchFilter,
        sort: SearchSort,
        partial: bool,
        k: usize,
    ) -> Result<(Vec<SearchHit>, bool)> {
        // Windows ON: this is the shape a reader's surface uses. A caller that
        // throws the snippet away should say so and ask
        // [`hybrid_search_legs`](Self::hybrid_search_legs) directly.
        let (hits, window_full) =
            self.hybrid_search_legs(account_id, query_text, filter, sort, partial, true, k)?;
        Ok((hits.into_iter().map(|h| h.hit).collect(), window_full))
    }

    /// [`hybrid_search`](Self::hybrid_search), plus WHICH LEG produced each hit.
    ///
    /// The human door reports this per item, because "the keyword leg and the
    /// vector leg both found this" and "only the vectors did" are different
    /// answers to how much to trust a result, and a client that cannot tell
    /// them apart cannot say so to the reader. The plain method above is the
    /// same search with the provenance dropped.
    ///
    /// `want_windows` BUYS ONE QUERY PER HIT, so a caller that drops the
    /// snippet says no. The agent door does: `search_mail` builds its result
    /// from the subject alone, and `k` there is `limit + offset` capped at 600,
    /// so windowing a deep page would compile and run hundreds of statements
    /// for a field nothing reads.
    #[allow(clippy::too_many_arguments)] // the query, the operators, the order, the window
    pub fn hybrid_search_legs(
        &self,
        account_id: AccountId,
        query_text: &str,
        filter: &SearchFilter,
        sort: SearchSort,
        partial: bool,
        want_windows: bool,
        k: usize,
    ) -> Result<(Vec<LeggedHit>, bool)> {
        self.hybrid_search_legs_ordered(
            account_id,
            query_text,
            filter,
            sort,
            partial,
            if want_windows { 0..usize::MAX } else { 0..0 },
            k,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn hybrid_search_legs_ordered(
        &self,
        account_id: AccountId,
        query_text: &str,
        filter: &SearchFilter,
        sort: SearchSort,
        partial: bool,
        snippet_range: std::ops::Range<usize>,
        k: usize,
    ) -> Result<(Vec<LeggedHit>, bool)> {
        // No embedder (e.g. before the background attach), or one that failed
        // on this query => keyword-only.
        let qvec = self
            .embedder()
            .and_then(|embedder| query_embed_or_degrade(embedder.as_ref(), query_text));
        let vec_hits: Vec<Candidate> = match qvec {
            Some(qvec) => {
                // sqlite-vec caps one nearest-neighbor query at 4096. For the
                // human door, which pages as deep as it likes, keyword recall
                // keeps expanding beyond the semantic window.
                self.knn_by_vector(account_id, &qvec, k.min(4096))?
                    .into_iter()
                    .map(|(c, _dist)| c)
                    .collect()
            }
            None => Vec::new(),
        };
        // FTS ranks over the SAME query text, sent mail included.
        let fts = FtsQuery::build(query_text, partial);
        let fts_hits = self.fts_recall(account_id, &fts, k, Audience::Internal)?;
        self.hybrid_fuse(
            account_id,
            &fts,
            vec_hits,
            fts_hits,
            filter,
            sort,
            snippet_range,
            k,
        )
    }

    /// The half of a hybrid search after its two legs: fuse them, cut the
    /// window at `k`, hydrate and apply the operators. Split out so an
    /// external search can hand over legs it has already cut down to readable
    /// rows, and fuse as many windows as it needs from one embed and one KNN.
    #[allow(clippy::too_many_arguments)]
    fn hybrid_fuse(
        &self,
        account_id: AccountId,
        fts: &FtsQuery,
        vec_hits: Vec<Candidate>,
        fts_hits: Vec<Candidate>,
        filter: &SearchFilter,
        sort: SearchSort,
        snippet_range: std::ops::Range<usize>,
        k: usize,
    ) -> Result<(Vec<LeggedHit>, bool)> {
        // ONE clock for both legs of one search.
        let now = Utc::now();

        let mut ranked = fuse_ranked(&[&vec_hits, &fts_hits], sort, now);
        ranked.truncate(k);
        // Judged BEFORE the filter drops anything: fullness is a property of
        // the recall window, not of what survived the operators.
        let window_full = ranked.len() == k;

        let from_fts: HashSet<i64> = fts_hits.iter().map(|c| c.id).collect();
        let from_vec: HashSet<i64> = vec_hits.iter().map(|c| c.id).collect();

        // ONE LOCK FOR THE WHOLE HYDRATION. Both recall legs are done, so
        // nothing below needs the connection back; taking and releasing the
        // store mutex per hit (twice per hit, with the window) would hand the
        // daemon's sync, triage and notify lanes hundreds of acquisitions to
        // interleave with, to serve one page.
        let conn = self.lock()?;
        let mut out = Vec::with_capacity(ranked.len());
        for c in ranked {
            if let Some(hit) = self.search_hit_by_id(&conn, account_id, c.id)? {
                if !filter.matches(&hit) {
                    continue;
                }
                out.push(LeggedHit {
                    keyword: from_fts.contains(&c.id),
                    vector: from_vec.contains(&c.id),
                    hit,
                });
            }
        }
        filter.order_hits(&mut out, |item| item.hit.is_done);
        for (index, item) in out.iter_mut().enumerate() {
            if snippet_range.contains(&index)
                && let Some(window) = self.fts_snippet(&conn, account_id, item.hit.id, &fts.any)?
            {
                window.apply(&mut item.hit);
            }
        }
        Ok((out, window_full))
    }

    /// The FTS match window for ONE message under `match_expr` (an expression
    /// [`FtsQuery`] built, never raw reader text), or `None` when there is
    /// nothing better than the stored snippet to show: the row is not in the
    /// index, the terms hit the subject rather than the body, or the MATCH
    /// expression is malformed. Every one of those keeps the caller's existing
    /// snippet, so a bad query degrades the preview instead of failing the
    /// search.
    ///
    /// The body-scoped subquery distinguishes body evidence from subject-only
    /// hits, even if a sender puts our cosmetic marker in their own body.
    /// Subject-only hits keep their stored snippet and report subject matches.
    ///
    /// `prepare_cached`, and the connection comes from the caller: this runs
    /// once per hydrated hit, up to `recall_k` (600) of them for a deep page.
    ///
    /// Account-scoped human snippet extraction. External results are rehydrated
    /// through external_search_hits and never reuse these cached match windows.
    fn fts_snippet(
        &self,
        conn: &Connection,
        account_id: AccountId,
        message_id: i64,
        match_expr: &str,
    ) -> Result<Option<MatchExcerpt>> {
        if match_expr.is_empty() {
            return Ok(None);
        }
        let sql = format!(
            "SELECT CASE WHEN f.rowid IN (
                 SELECT rowid FROM messages_fts WHERE rowid = ?1 AND messages_fts MATCH ?4
             ) THEN {BODY_WINDOW} END,
             highlight(messages_fts, 0, char(1), char(2)),
             CASE WHEN f.rowid IN (
                 SELECT rowid FROM messages_fts WHERE rowid = ?1 AND messages_fts MATCH ?5
             ) THEN {TBODY_WINDOW} END
             FROM messages_fts f
             CROSS JOIN messages m ON m.id = f.rowid
             LEFT JOIN triage t ON t.message_id = m.id
             WHERE f.rowid = ?1
               AND m.account_id = ?2
               AND m.is_spam = 0
               AND messages_fts MATCH ?3"
        );
        let mut stmt = match conn.prepare_cached(&sql) {
            Ok(s) => s,
            Err(_) => return Ok(None),
        };
        // A syntactically-invalid MATCH errors at step time, not prepare time;
        // both collapse to "no window".
        let scoped = format!("body : ({match_expr})");
        let translated = format!("tbody : ({match_expr})");
        stmt.query_row(
            params![message_id, account_id, match_expr, scoped, translated],
            |r| {
                // The original body's window first; the translation's only when
                // the terms are in the English and nowhere in the original.
                let body: Option<String> = r.get::<_, Option<String>>(0)?.or(r.get(2)?);
                let subject: String = r.get(1)?;
                Ok(MatchExcerpt {
                    snippet: body
                        .as_deref()
                        .map(|raw| raw.replace([SNIPPET_MARK, '\u{2}'], "")),
                    snippet_matches: body.as_deref().map(matched_words).unwrap_or_default(),
                    subject_matches: matched_words(&subject),
                })
            },
        )
        .optional()
        .or(Ok(None))
    }

    /// SEMANTIC-ONLY recall as hydrated [`SearchHit`]s for the human door's
    /// `mode=semantic` search: the KNN window, reordered by distance rank AND
    /// recency (see [`fuse_ranked`], which a one-list call reduces to exactly
    /// that). Errors without an attached embedder. Sealed rows are excluded in
    /// SQL; sent mail is included (recall). Snippets stay the stored
    /// head-of-message text: a vector hit matched by meaning, so there is no
    /// term window to cut around.
    ///
    /// Recency reorders WITHIN the KNN window and cannot reach outside it —
    /// nothing the vector index did not return can be lifted in by being fresh.
    ///
    /// APPROXIMATION: `filter` narrows the top-`k` window AFTER ranking, since
    /// KNN cannot carry a `from:`/date predicate. A heavily-filtered query can
    /// therefore under-fill a page even when more matching mail exists deeper in
    /// the index; callers over-fetch `k` to soften it (see the door handler).
    ///
    /// The second return value is WINDOW FULL — same contract as
    /// [`hybrid_search`](Self::hybrid_search).
    pub fn semantic_search_hits(
        &self,
        account_id: AccountId,
        query_text: &str,
        filter: &SearchFilter,
        sort: SearchSort,
        partial: bool,
        k: usize,
    ) -> Result<(Vec<SearchHit>, bool)> {
        let knn = self.semantic_knn(account_id, query_text, k)?;
        let window_full = knn.len() == k;
        let candidates: Vec<Candidate> = knn.into_iter().map(|(c, _dist)| c).collect();
        // The vector leg matched by meaning, but the mail it found may still
        // SAY one of the reader's words, and if it does that sentence is worth
        // more than the head of the message. Asked with the ANY expression, so
        // a single term landing anywhere in the body wins a window.
        let fts = FtsQuery::build(query_text, partial);
        let hits = self.semantic_hits_from(account_id, &candidates, Some(&fts), filter, sort)?;
        Ok((hits, window_full))
    }

    /// The half of a semantic search after the KNN: order `candidates` by
    /// distance rank and recency, hydrate, apply the operators. `windows` is
    /// the query whose term windows replace stored snippets; `None` for a
    /// caller that throws the snippet away.
    fn semantic_hits_from(
        &self,
        account_id: AccountId,
        candidates: &[Candidate],
        windows: Option<&FtsQuery>,
        filter: &SearchFilter,
        sort: SearchSort,
    ) -> Result<Vec<SearchHit>> {
        let ranked = fuse_ranked(&[candidates], sort, Utc::now());
        // ONE LOCK for the whole hydration; see the twin loop in
        // [`hybrid_search_legs`](Self::hybrid_search_legs).
        let conn = self.lock()?;
        let mut out = Vec::with_capacity(ranked.len());
        for c in ranked {
            if let Some(mut hit) = self.search_hit_by_id(&conn, account_id, c.id)?
                && filter.matches(&hit)
            {
                if let Some(fts) = windows
                    && let Some(window) = self.fts_snippet(&conn, account_id, c.id, &fts.any)?
                {
                    window.apply(&mut hit);
                }
                out.push(hit);
            }
        }
        filter.order_hits(&mut out, |hit| hit.is_done);
        Ok(out)
    }

    /// FTS5 recall helper for [`hybrid_search`]: candidates in bm25 rank order,
    /// STRICT MATCHES FIRST and then any-only ones, exactly the order
    /// [`search_filtered`](Self::search_filtered) serves. The two legs share a
    /// builder AND a running order, so keyword mode and hybrid mode cannot
    /// disagree about what a query means.
    ///
    /// Unlike [`Store::search`] it INCLUDES sent mail, because recall wants the
    /// user's own outbound mail. Sealed rows are excluded in SQL, and a malformed
    /// FTS query yields an empty list rather than an error.
    ///
    /// PURE RELEVANCE ORDER, unlike the keyword leg's own `ORDER BY`: this list
    /// is an INPUT to the fusion, which applies the recency vote once, across
    /// every leg. Blending it in here too would count it twice.
    fn fts_recall(
        &self,
        account_id: AccountId,
        fts: &FtsQuery,
        limit: usize,
        audience: Audience,
    ) -> Result<Vec<Candidate>> {
        if fts.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.lock()?;
        let mut out =
            self.fts_recall_pass(&conn, account_id, &fts.strict, None, limit, audience)?;
        // One term means the two expressions are identical; anything else and
        // the any-only pass fills the rest of the window BELOW the strict hits.
        if fts.terms.len() > 1 && out.len() < limit {
            let rest = self.fts_recall_pass(
                &conn,
                account_id,
                &fts.any,
                Some(&fts.strict),
                limit - out.len(),
                audience,
            )?;
            out.extend(rest);
        }
        Ok(out)
    }

    /// One pass of [`fts_recall`](Self::fts_recall): the rows matching `expr`
    /// minus those matching `exclude`, in bm25 order.
    fn fts_recall_pass(
        &self,
        conn: &Connection,
        account_id: AccountId,
        expr: &str,
        exclude: Option<&str>,
        limit: usize,
        audience: Audience,
    ) -> Result<Vec<Candidate>> {
        let mut sql = String::from(
            "SELECT m.id, m.received_at
             FROM messages_fts f
             CROSS JOIN messages m ON m.id = f.rowid
             LEFT JOIN triage t ON t.message_id = m.id
             WHERE m.account_id = ?
               AND m.is_spam = 0
               AND messages_fts MATCH ?",
        );
        sql.push_str(audience.clause());
        let mut args = vec![Value::Integer(account_id), Value::Text(expr.to_string())];
        if let Some(exclude) = exclude {
            // The subquery carries no sealed/spam guard and needs none: it only
            // SUBTRACTS from a set the outer WHERE has already narrowed, so the
            // worst it could do is hide a row, never reveal one.
            sql.push_str(
                " AND f.rowid NOT IN (
                     SELECT rowid FROM messages_fts WHERE messages_fts MATCH ?)",
            );
            args.push(Value::Text(exclude.to_string()));
        }
        // bm25 ascending is best-first (the score is negative), which is what
        // `ORDER BY rank` meant before the subject weight moved the ranking off
        // the table's default.
        //
        // An external window also breaks bm25 ties, newest first. Without it, which of
        // several equal matches falls inside the LIMIT is whatever order the
        // doclist is walked in, and that is one more thing about a result
        // that should not be left to mail the caller cannot see.
        sql.push_str(&match audience {
            Audience::Internal => format!(" ORDER BY {BM25} LIMIT ?"),
            Audience::External => {
                format!(" ORDER BY {BM25}, m.received_at DESC, m.id DESC LIMIT ?")
            }
        });
        args.push(Value::Integer(limit as i64));
        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => s,
            Err(_) => return Ok(Vec::new()),
        };
        let rows = stmt.query_map(params_from_iter(args), |r| {
            Ok(Candidate {
                id: r.get(0)?,
                received_at: dt(r, 1)?,
            })
        });
        let rows = match rows {
            Ok(r) => r,
            // A syntactically-invalid MATCH expression => no keyword hits.
            Err(_) => return Ok(Vec::new()),
        };
        let mut out = Vec::new();
        for row in rows {
            match row {
                Ok(c) => out.push(c),
                Err(_) => return Ok(out),
            }
        }
        Ok(out)
    }

    /// Hydrate one account-owned message for human/internal recall.
    ///
    /// The connection comes from the caller and the statement is cached: this
    /// runs once per candidate in a hydration loop, so a lock per call and a
    /// fresh compile per call are both paid `recall_k` times over.
    fn search_hit_by_id(
        &self,
        conn: &Connection,
        account_id: AccountId,
        id: i64,
    ) -> Result<Option<SearchHit>> {
        let row = conn
            .prepare_cached(concat!(
                "SELECT ",
                hit_columns_sql!(),
                " FROM messages m
                 LEFT JOIN triage t ON t.message_id = m.id
                 WHERE m.account_id = ?1 AND m.id = ?2
                       AND m.is_spam = 0",
            ))?
            .query_row(params![account_id, id], map_search_hit)
            .optional()?;
        Ok(row)
    }

    /// Unfiltered keyword search: [`search_filtered`](Self::search_filtered)
    /// with no operators. The plain-query entry point every non-door caller
    /// uses.
    pub(super) fn search(
        &self,
        account_id: AccountId,
        query: &str,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<SearchHit>> {
        self.search_filtered(
            account_id,
            query,
            &SearchFilter::default(),
            SearchSort::default(),
            false,
            limit,
            offset,
        )
    }

    /// ONE PAGE OF THE KEYWORD LEG: the messages matching `expr`, MINUS those
    /// matching `exclude` when it is given, ranked by bm25 blended with recency
    /// (see the sort-key comment in [`search_filtered`](Self::search_filtered)).
    ///
    /// `exclude` is what makes the any-term pass an ANY-ONLY pass: the rows the
    /// strict pass already served are subtracted here rather than deduplicated
    /// in Rust, so LIMIT/OFFSET keep cutting an exact page out of a real
    /// ordering instead of out of a fetched window.
    ///
    /// Both passes share account, spam, and sent-mail filters. External access
    /// is checked separately when an external consumer requests these results.
    #[allow(clippy::too_many_arguments)] // one keyword query, one argument per part
    fn keyword_page(
        &self,
        conn: &Connection,
        account_id: AccountId,
        expr: &str,
        exclude: Option<&str>,
        filter: &SearchFilter,
        sort: SearchSort,
        limit: u32,
        offset: u32,
        audience: Audience,
    ) -> Result<Vec<SearchHit>> {
        // The trailing column is the FTS match window; the mapper swaps it in
        // over the stored head-of-message snippet only when the body really
        // matched (see `body_window`) — a subject-only hit keeps the curated
        // snippet.
        let mut sql = format!(
            "SELECT {HIT_COLUMNS},
                    highlight(messages_fts, 0, char(1), char(2)), {BODY_WINDOW}, {TBODY_WINDOW}
             FROM messages_fts f
             CROSS JOIN messages m ON m.id = f.rowid
             LEFT JOIN triage t ON t.message_id = m.id
             WHERE m.account_id = ?
               AND m.is_spam = 0
               AND messages_fts MATCH ?"
        );
        sql.push_str(audience.clause());
        let mut args = vec![Value::Integer(account_id), Value::Text(expr.to_string())];
        if let Some(exclude) = exclude {
            // The subquery carries no sealed/spam guard and needs none: it only
            // SUBTRACTS from a set the outer WHERE has already narrowed, so the
            // worst it could do is hide a row, never reveal one.
            sql.push_str(
                " AND f.rowid NOT IN (
                     SELECT rowid FROM messages_fts WHERE messages_fts MATCH ?)",
            );
            args.push(Value::Text(exclude.to_string()));
        }
        if !filter.searches_sent() {
            sql.push_str(" AND m.is_sent = 0");
        }
        push_filter_clauses(&mut sql, &mut args, filter);
        // THE SORT KEY. Both branches are BIGGEST FIRST — bm25 is NEGATIVE
        // (more negative = better), so `-bm25(...)` is the relevance, which is
        // what lets the tiebreakers below read the same way under either one.
        //
        // RECENCY IS BLENDED IN SQL, not in Rust. This leg paginates with
        // LIMIT/OFFSET and must keep doing that exactly; re-ranking a fetched
        // window would turn exact pagination into the recall legs' over-fetch
        // approximation for no reason.
        //
        // MULTIPLICATIVE, not additive. bm25's magnitude swings by orders of
        // magnitude with how many terms the reader typed and how rare they are
        // — the scores in one result set are comparable only to each other. An
        // additive recency bonus would therefore drown one query and vanish
        // under the next; a factor means the same thing at every scale.
        let relevance = if sort.considers_recency() {
            format!("(-{BM25}) * {}", recency::boost_sql("m.received_at", "?"))
        } else {
            format!("(-{BM25})")
        };
        sql.push_str(&format!(
            " ORDER BY {relevance} DESC, m.received_at DESC, m.id DESC LIMIT ? OFFSET ?"
        ));
        // The clock is pushed HERE, after the filter's parameters and before
        // LIMIT/OFFSET, because anonymous `?` are numbered in the order they
        // appear in the SQL TEXT and ORDER BY is parsed after WHERE. Under
        // BestMatch the expression has no placeholder, so neither may the args.
        if sort.considers_recency() {
            args.push(Value::Text(Utc::now().to_rfc3339()));
        }
        args.push(Value::Integer(limit as i64));
        args.push(Value::Integer(offset as i64));

        let mut stmt = conn.prepare(&sql)?;
        // A syntactically-invalid MATCH expression errors at STEP time, after a
        // clean prepare. `FtsQuery` makes that unreachable from a search box,
        // but the sibling legs read a bad MATCH as "no keyword hits" and this
        // one must agree, or the same bad query 200s in hybrid mode and 500s in
        // keyword mode.
        let rows = match stmt.query_map(params_from_iter(args), map_search_hit_with_window) {
            Ok(rows) => rows,
            Err(_) => return Ok(Vec::new()),
        };
        let mut out = Vec::new();
        for row in rows {
            match row {
                Ok(hit) => out.push(hit),
                Err(_) => return Ok(out),
            }
        }
        Ok(out)
    }

    /// How many messages the STRICT expression matches under exactly the
    /// predicates [`keyword_page`](Self::keyword_page) applies. The offset the
    /// any-only pass starts from is `offset - this`, so the two passes join
    /// without a gap and without a repeat.
    fn keyword_total(
        &self,
        conn: &Connection,
        account_id: AccountId,
        expr: &str,
        filter: &SearchFilter,
        audience: Audience,
    ) -> Result<u32> {
        let mut sql = String::from(
            "SELECT COUNT(*)
             FROM messages_fts f
             CROSS JOIN messages m ON m.id = f.rowid
             LEFT JOIN triage t ON t.message_id = m.id
             WHERE m.account_id = ?
               AND m.is_spam = 0
               AND messages_fts MATCH ?",
        );
        sql.push_str(audience.clause());
        let mut args = vec![Value::Integer(account_id), Value::Text(expr.to_string())];
        if !filter.searches_sent() {
            sql.push_str(" AND m.is_sent = 0");
        }
        push_filter_clauses(&mut sql, &mut args, filter);
        let mut stmt = conn.prepare(&sql)?;
        // THIS ONE PROPAGATES, unlike every sibling MATCH on this leg, and the
        // asymmetry is deliberate. The siblings read a query error as "no
        // hits", which is a truthful degradation of a LIST. This count is not
        // a list: it is the seam between the strict page and the any-only one,
        // and a zero here is not "nothing matched" but "start the any-only
        // pass at the wrong offset". A count that failed while the page beside
        // it succeeded (different statements, so a busy timeout is enough)
        // would skip rows on page two and serve the strict block nowhere at
        // all. Exact pagination is the claim; failing loudly is how it stays
        // one.
        let n: i64 = stmt.query_row(params_from_iter(args), |r| r.get(0))?;
        Ok(n.max(0) as u32)
    }

    /// KEYWORD PATH with the operator half applied in SQL. `text` is already
    /// parsed (see [`crate::store::parse_search_query`]); empty text plus a
    /// filter routes to [`filter_only_listing`](Self::filter_only_listing).
    ///
    /// AND, THEN OR. The page is the STRICT matches (every term) in rank order,
    /// followed by the mail carrying only SOME of the terms, in rank order,
    /// with the strict rows subtracted so nothing appears twice. A message that
    /// matches everything therefore can never sink below one that matches less,
    /// and a query with one word the right mail happens to lack stops returning
    /// nothing at all. See [`FtsQuery::build`] for the mailbox that taught us.
    ///
    /// Ranked by bm25, SCALED BY RECENCY under [`SearchSort::Recent`] (see
    /// [`crate::store::recency`]) and left alone under
    /// [`SearchSort::BestMatch`]. PAGINATION STAYS EXACT across the boundary
    /// between the two passes: the strict pass is counted, the page is cut out
    /// of it by LIMIT/OFFSET, and whatever the page still owes is taken from
    /// the any-only pass at `offset - strict_total`.
    ///
    /// `partial` matches the LAST term as a prefix, for the panel's
    /// as-you-type fetch.
    #[allow(clippy::too_many_arguments)] // the query, the operators, the order, the page
    pub(super) fn search_filtered(
        &self,
        account_id: AccountId,
        text: &str,
        filter: &SearchFilter,
        sort: SearchSort,
        partial: bool,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<SearchHit>> {
        Ok(self
            .search_filtered_counted(account_id, text, filter, sort, partial, limit, offset)?
            .0)
    }

    /// Desktop search: status groups precede the strict/partial seam and
    /// LIMIT/OFFSET. Corpus scope comes from the same filter as diagnostics.
    #[allow(clippy::too_many_arguments)]
    pub fn search_unfinished_first(
        &self,
        account_id: AccountId,
        text: &str,
        filter: &SearchFilter,
        sort: SearchSort,
        partial: bool,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<SearchHit>> {
        let conn = self.lock()?;
        let fts = FtsQuery::build(text, partial);
        // Validate the caller's filter before adding our internal status predicate.
        if fts.is_empty() && filter.is_empty() {
            return self
                .search_filtered_locked(
                    &conn,
                    account_id,
                    text,
                    filter,
                    sort,
                    partial,
                    limit,
                    offset,
                    Audience::Internal,
                )
                .map(|(hits, _)| hits);
        }
        let mut group = filter.clone();
        group.done = Some(false);
        let unfinished = if offset == 0 {
            0 // No seam count is needed to fill the first page.
        } else if fts.is_empty() {
            let mut sql = String::from(
                "SELECT COUNT(*) FROM messages m LEFT JOIN triage t ON t.message_id = m.id WHERE m.account_id = ? AND m.is_spam = 0",
            );
            let mut args = vec![Value::Integer(account_id)];
            if !group.searches_sent() {
                sql.push_str(" AND m.is_sent = 0");
            }
            push_filter_clauses(&mut sql, &mut args, &group);
            conn.query_row(&sql, params_from_iter(args), |r| r.get::<_, u32>(0))?
        } else {
            self.keyword_total(&conn, account_id, &fts.any, &group, Audience::Internal)?
        };
        let mut out = self
            .search_filtered_locked(
                &conn,
                account_id,
                text,
                &group,
                sort,
                partial,
                limit,
                offset,
                Audience::Internal,
            )?
            .0;
        let owed = limit.saturating_sub(out.len() as u32);
        if owed > 0 {
            group.done = Some(true);
            out.extend(
                self.search_filtered_locked(
                    &conn,
                    account_id,
                    text,
                    &group,
                    sort,
                    partial,
                    owed,
                    offset.saturating_sub(unfinished),
                    Audience::Internal,
                )?
                .0,
            );
        }
        Ok(out)
    }

    /// [`search_filtered`](Self::search_filtered) plus THE STRICT COUNT IT
    /// ALREADY TOOK, for a caller that would otherwise take it again.
    ///
    /// Beyond page zero, the seam between strict and any-only is a `COUNT(*)`
    /// over the strict expression (see [`keyword_total`](Self::keyword_total)),
    /// and the diagnostics beside the page report that same number as
    /// `strict_hits`. Counting it twice per request is one full doclist walk
    /// more than the question needs, under the store mutex that sync, triage
    /// and notify also queue on.
    ///
    /// `None` when no count was taken, which is not the same as zero: a
    /// one-term query needs no seam (strict and any are the same expression)
    /// and a filter-only listing runs no MATCH at all.
    ///
    /// THE COUNT IS NOT UNIVERSALLY REUSABLE, and the caller owns that. It
    /// carries this query's operator predicates, so it answers the diagnostics'
    /// question ("how many messages match every term") only when there are no
    /// operators; with a `from:` or a date bound the two are different
    /// questions and both deserve their own count. It also excludes the
    /// reader's own sent mail, exactly as keyword mode's hits do, so it is the
    /// keyword leg's number and not the recall legs'.
    #[allow(clippy::too_many_arguments)] // the query, the operators, the order, the page
    pub fn search_filtered_counted(
        &self,
        account_id: AccountId,
        text: &str,
        filter: &SearchFilter,
        sort: SearchSort,
        partial: bool,
        limit: u32,
        offset: u32,
    ) -> Result<(Vec<SearchHit>, Option<u32>)> {
        let conn = self.lock()?;
        self.search_filtered_locked(
            &conn,
            account_id,
            text,
            filter,
            sort,
            partial,
            limit,
            offset,
            Audience::Internal,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn search_filtered_locked(
        &self,
        conn: &Connection,
        account_id: AccountId,
        text: &str,
        filter: &SearchFilter,
        sort: SearchSort,
        partial: bool,
        limit: u32,
        offset: u32,
        audience: Audience,
    ) -> Result<(Vec<SearchHit>, Option<u32>)> {
        let fts = FtsQuery::build(text, partial);
        if fts.is_empty() {
            // No text AND no filter is not a search — it is "page me the whole
            // mailbox", which no caller means. Refusing here keeps the old
            // `search("")`-errors contract instead of silently listing mail
            // for the next caller who forgets to validate.
            if filter.is_empty() {
                // The reader typed SOMETHING the index cannot hold ("???"),
                // which is an honest empty result rather than an error and
                // definitely not an invitation to list the mailbox.
                if text.trim().is_empty() {
                    return Err(CoreError::InvalidInput("empty search query".into()));
                }
                return Ok((Vec::new(), None));
            }
            // No MATCH ran, so there is no strict set and nothing to report a
            // count of.
            let hits =
                self.filter_only_listing(conn, account_id, filter, limit, offset, audience)?;
            return Ok((hits, None));
        }
        // ONE TERM means strict and any are the same expression, so there is no
        // any-only pass to run and no count to take: the strict page IS the
        // page. Skipping both is not just an optimisation — a `NOT IN` against
        // an identical MATCH would correctly return nothing, and paying for two
        // more FTS scans to learn that is silly.
        if fts.terms.len() == 1 {
            let hits = self.keyword_page(
                conn,
                account_id,
                &fts.strict,
                None,
                filter,
                sort,
                limit,
                offset,
                audience,
            )?;
            return Ok((hits, None));
        }
        if offset == 0 {
            let mut hits = self.keyword_page(
                conn,
                account_id,
                &fts.strict,
                None,
                filter,
                sort,
                limit,
                0,
                audience,
            )?;
            if hits.len() == limit as usize {
                return Ok((hits, None));
            }
            let strict_total = hits.len() as u32;
            hits.extend(self.keyword_page(
                conn,
                account_id,
                &fts.any,
                Some(&fts.strict),
                filter,
                sort,
                limit - strict_total,
                0,
                audience,
            )?);
            return Ok((hits, Some(strict_total)));
        }
        let strict_total = self.keyword_total(conn, account_id, &fts.strict, filter, audience)?;
        let mut out = Vec::new();
        if offset < strict_total {
            out = self.keyword_page(
                conn,
                account_id,
                &fts.strict,
                None,
                filter,
                sort,
                limit,
                offset,
                audience,
            )?;
        }
        // What the page still owes comes off the any-only ranking. Its offset
        // is how far past the strict block this page starts (zero when the page
        // straddles the boundary, because the strict rows above have already
        // been served).
        let owed = limit as usize - out.len().min(limit as usize);
        if owed > 0 {
            let any_offset = offset.saturating_sub(strict_total);
            let rest = self.keyword_page(
                conn,
                account_id,
                &fts.any,
                Some(&fts.strict),
                filter,
                sort,
                owed as u32,
                any_offset,
                audience,
            )?;
            out.extend(rest);
        }
        Ok((out, Some(strict_total)))
    }

    /// WHAT RETRIEVAL MADE OF THE READER'S WORDS: how many messages match every
    /// term, how many match any term, and the document frequency of each term
    /// on its own. The door reports this beside the hits; §5 of docs/SEARCH.md
    /// is what reads it.
    ///
    /// `include_sent` FOLLOWS THE LEG THAT RAN, and the caller passes what its
    /// own mode did: keyword mode excludes the user's sent mail from its hits,
    /// the recall legs include it. Counts that disagreed with the list beside
    /// them would be worse than no counts, because they would look like a
    /// missing page rather than a different question.
    ///
    /// EVERY COUNT STOPS AT [`DIAGNOSTIC_COUNT_CAP`]. This runs on every
    /// request, the panel's as-you-type ones included, so an exact frequency is
    /// not worth a full doclist walk under the store mutex; nothing that reads
    /// these needs one.
    ///
    /// Human-only diagnostics include restricted mail and exclude spam, exactly
    /// like the search results. These counts must never be exposed to agents.
    pub fn search_diagnostics(
        &self,
        account_id: AccountId,
        text: &str,
        partial: bool,
        include_sent: bool,
    ) -> Result<SearchDiagnostics> {
        self.search_diagnostics_with(account_id, text, partial, include_sent, None)
    }

    /// [`search_diagnostics`](Self::search_diagnostics), told a strict count
    /// the caller has ALREADY TAKEN.
    ///
    /// The keyword page counts its own strict set to place the seam between the
    /// two passes ([`search_filtered_counted`](Self::search_filtered_counted)),
    /// and that is the same number `strict_hits` reports. Handing it over turns
    /// two doclist walks per request into one.
    ///
    /// WHAT THE CALLER IS PROMISING, because nothing here can check it: that
    /// the number came from the SAME question these counts ask — this account,
    /// no operator predicates, provider spam excluded, and sent mail excluded
    /// exactly as `include_sent = false` would. A count taken under a `from:`
    /// belongs to a different set and must not be passed; the two predicates
    /// differ and two counts are then the honest answer.
    ///
    /// It is CLAMPED to [`DIAGNOSTIC_COUNT_CAP`] on the way in, because the
    /// page's count is exact and everything the door reports here stops at the
    /// cap. A `strict_hits` above a capped `any_hits` would read as a mailbox
    /// where more messages match every term than match any of them.
    pub fn search_diagnostics_with(
        &self,
        account_id: AccountId,
        text: &str,
        partial: bool,
        include_sent: bool,
        counted_strict: Option<u32>,
    ) -> Result<SearchDiagnostics> {
        let fts = FtsQuery::build(text, partial);
        if fts.is_empty() {
            // Operators only, or punctuation only: nothing was looked up, and
            // zero is the honest report rather than a count of the mailbox.
            return Ok(SearchDiagnostics::default());
        }
        let conn = self.lock()?;
        let strict_hits = match counted_strict {
            Some(n) => n.min(DIAGNOSTIC_COUNT_CAP),
            None => self.fts_count(&conn, account_id, &fts.strict, include_sent)?,
        };
        // A ONE-TERM QUERY IS ONE EXPRESSION THREE TIMES: strict, any and the
        // term itself are the same string, and this count is a pure function of
        // (account, expression, include_sent). Asking SQLite the identical
        // question twice more would only cost the store mutex.
        let any_hits = if fts.any == fts.strict {
            strict_hits
        } else {
            self.fts_count(&conn, account_id, &fts.any, include_sent)?
        };
        let mut terms = Vec::with_capacity(fts.terms.len());
        for (term, expr) in fts.terms.iter().zip(fts.term_exprs.iter()) {
            // Each term is counted BY THE EXPRESSION THAT RANKED IT, straight
            // off the builder — with `partial` on, the tail matched a whole OR
            // group, and a df for the bare word would describe a search nobody
            // ran. Rebuilding the string here is what let the two drift.
            let df = if expr == &fts.strict {
                strict_hits
            } else {
                self.fts_count(&conn, account_id, expr, include_sent)?
            };
            terms.push(TermDf {
                text: term.clone(),
                df,
            });
        }
        Ok(SearchDiagnostics {
            strict_hits,
            any_hits,
            terms,
        })
    }

    /// How many non-spam messages of this account match `expr`,
    /// COUNTED NO FURTHER THAN [`DIAGNOSTIC_COUNT_CAP`]. The counting half of
    /// [`search_diagnostics`](Self::search_diagnostics); no operators, because
    /// a count of "what the index holds" is not a count of one filtered page.
    ///
    /// The cap is what makes this affordable on every keystroke. An uncapped
    /// `COUNT(*)` over a MATCH walks the whole doclist, and this runs
    /// `terms + 2` times per request while holding the store mutex that sync,
    /// triage and notify also queue on — the shape of the p95 floor the
    /// kill-p95 branch had to remove. Nothing reads an exact frequency: §5 of
    /// docs/SEARCH.md asks "did anything match all of it", and the panel asks
    /// "is this word rare". Zero, some and many is the whole vocabulary.
    fn fts_count(
        &self,
        conn: &Connection,
        account_id: AccountId,
        expr: &str,
        include_sent: bool,
    ) -> Result<u32> {
        let sql = fts_count_sql(include_sent);
        // Same reading as every other MATCH on this leg.
        let n: i64 = conn
            .query_row(
                &sql,
                params![account_id, expr, DIAGNOSTIC_COUNT_CAP as i64],
                |r| r.get(0),
            )
            .unwrap_or(0);
        Ok(n.max(0) as u32)
    }

    /// FILTER-ONLY LISTING: the reader typed operators and nothing else
    /// (`from:jane after:2026-01-01`), so there is no text to rank on and no
    /// MATCH to run — this is a plain newest-first page over `messages`.
    ///
    /// Account-scoped human listing. Sent mail and provider spam are excluded;
    /// pending and restricted inbound messages remain readable.
    fn filter_only_listing(
        &self,
        conn: &Connection,
        account_id: AccountId,
        filter: &SearchFilter,
        limit: u32,
        offset: u32,
        audience: Audience,
    ) -> Result<Vec<SearchHit>> {
        let mut sql = format!(
            "SELECT {HIT_COLUMNS}
             FROM messages m
             LEFT JOIN triage t ON t.message_id = m.id
             WHERE m.account_id = ?
               AND m.is_spam = 0"
        );
        sql.push_str(audience.clause());
        let mut args = vec![Value::Integer(account_id)];
        if !filter.searches_sent() {
            sql.push_str(" AND m.is_sent = 0");
        }
        push_filter_clauses(&mut sql, &mut args, filter);
        // The id tiebreaker matters: Date headers are second-resolution, so a
        // list blast ties routinely, and OFFSET over an unstable sort drops and
        // repeats rows across page boundaries.
        sql.push_str(" ORDER BY m.received_at DESC, m.id DESC LIMIT ? OFFSET ?");
        args.push(Value::Integer(limit as i64));
        args.push(Value::Integer(offset as i64));

        let mut stmt = conn.prepare(&sql)?;
        let out = stmt
            .query_map(params_from_iter(args), map_search_hit)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(out)
    }

    pub(super) fn upsert_message_vector(
        &self,
        account_id: AccountId,
        message_id: i64,
        embedding: &[f32],
    ) -> Result<()> {
        self.upsert_message_vectors(account_id, &[(message_id, embedding)])
    }

    /// Every vector in `items`, under ONE commit. vec0 rejects a re-INSERT on
    /// an existing rowid, so an idempotent write is a DELETE then an INSERT,
    /// and in autocommit that was two commits per vector, two fsyncs at
    /// `synchronous=FULL`: a backfill batch of eight paid sixteen. Here the
    /// batch pays one, and a single vector (ingest's embed-at-write) one
    /// instead of two. All or nothing: a bad width anywhere in the batch
    /// writes nothing.
    pub(super) fn upsert_message_vectors(
        &self,
        account_id: AccountId,
        items: &[(i64, &[f32])],
    ) -> Result<()> {
        for (_, embedding) in items {
            if embedding.len() != VEC_DIMS {
                return Err(CoreError::InvalidInput(format!(
                    "embedding len {} != vec0 width {VEC_DIMS}",
                    embedding.len()
                )));
            }
        }
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        {
            let mut delete = tx.prepare_cached("DELETE FROM message_vecs WHERE message_id = ?1")?;
            let mut insert = tx.prepare_cached(
                "INSERT INTO message_vecs(message_id, embedding, account_id)
                 VALUES (?1, ?2, ?3)",
            )?;
            for (message_id, embedding) in items {
                delete.execute(params![message_id])?;
                insert.execute(params![message_id, embedding.as_bytes(), account_id])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// [`Self::upsert_message_vector`] for the English translation's vector
    /// (`message_translation_vecs`). Written only while the row's translation
    /// is served: a translation dropped or closed meanwhile gets no vector,
    /// and the check and the write share one lock.
    pub(super) fn upsert_translation_vector(
        &self,
        account_id: AccountId,
        message_id: i64,
        embedding: &[f32],
    ) -> Result<()> {
        if embedding.len() != VEC_DIMS {
            return Err(CoreError::InvalidInput(format!(
                "embedding len {} != vec0 width {VEC_DIMS}",
                embedding.len()
            )));
        }
        let conn = self.lock()?;
        let served: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM message_translations tr
                 WHERE tr.message_id = ?1 AND tr.account_id = ?2
                   AND tr.status IN ('done','incomplete'))",
            params![message_id, account_id],
            |r| r.get(0),
        )?;
        conn.execute(
            "DELETE FROM message_translation_vecs WHERE message_id = ?1",
            params![message_id],
        )?;
        if served {
            conn.execute(
                "INSERT INTO message_translation_vecs(message_id, embedding, account_id)
                 VALUES (?1, ?2, ?3)",
                params![message_id, embedding.as_bytes(), account_id],
            )?;
        }
        Ok(())
    }

    /// Up to `limit` served translations with no vector yet, newest mail
    /// first, as the TRANSLATED subject and body. Spam is excluded like the
    /// original's drain.
    pub(super) fn translations_missing_vectors(
        &self,
        account_id: AccountId,
        limit: usize,
    ) -> Result<Vec<MissingVector>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT m.id, COALESCE(tr.subject, ''), COALESCE(tr.body, '')
             FROM message_translations tr
             JOIN messages m ON m.id = tr.message_id
             WHERE tr.account_id = ?1
               AND tr.status IN ('done','incomplete')
               AND m.is_spam = 0
               AND NOT EXISTS (
                   SELECT 1 FROM message_translation_vecs v WHERE v.message_id = m.id
               )
             ORDER BY m.received_at DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![account_id, limit as i64], |r| {
            Ok(MissingVector {
                message_id: r.get(0)?,
                subject: r.get(1)?,
                body: r.get(2)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub(super) fn messages_missing_vectors(
        &self,
        account_id: AccountId,
        limit: usize,
    ) -> Result<Vec<MissingVector>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT m.id, m.subject, m.body
             FROM messages m
             JOIN triage t ON t.message_id = m.id
             WHERE m.account_id = ?1
               AND m.is_spam = 0
               AND NOT EXISTS (
                   SELECT 1 FROM message_vecs v WHERE v.message_id = m.id
               )
             ORDER BY m.received_at DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![account_id, limit as i64], |r| {
            Ok(MissingVector {
                message_id: r.get(0)?,
                subject: r.get(1)?,
                body: r.get(2)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }
}
