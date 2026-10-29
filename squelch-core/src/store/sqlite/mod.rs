//! SQLite-backed [`Store`] implementation.
//!
//! rusqlite is synchronous, so the `Connection` is wrapped in a `Mutex` and the
//! trait is implemented synchronously. See `store/mod.rs` for rationale.
//!
//! This file holds the struct, the open/migrate/attach glue and the single
//! [`Store`] impl block; the query bodies live in the subject modules declared
//! below, as inherent methods that block delegates to. A trait impl cannot be
//! split across files, which is why the delegations exist.

/// AUTHORED BY THIS ACCOUNT, as SQL over a `messages m` row: the stored sent
/// copy, OR a copy in the inbound listing that the account's own mailbox wrote.
///
/// Stored `messages.is_sent` alone is not that. It is a VISIBILITY flag, sticky
/// to 0, and the sync engine lets the INBOX sighting win whenever a message
/// carries both labels: mail to oneself, a self-Cc, and any sent message a mail
/// client re-labels INBOX (a "remind me if nobody replies" does exactly that).
/// Read as inbound, the user's own question becomes an inquiry FROM a known
/// contact, and the model has filed "reply to <the user>" in FYE on a real
/// mailbox. So every "is this inbound mail the user might owe something on"
/// the agent-triage store asks is asked of this, never of the bare column:
/// the user's own mail is evidence about a thread and is not shown back to
/// them.
///
/// THE FROM ADDRESS IS NOT ENOUGH, because anybody can type it. What this arm
/// asserts to the model is "the user said this", which resolves obligations
/// silently, so a forged `From: <the user>` must not earn it. `auth_pass IS
/// NULL` is the test: a copy the mailbox wrote never crossed the provider's
/// inbound SMTP edge and carries no delivery verdict, while mail delivered
/// from outside is stamped with one, pass or fail (see `extract_auth_pass`).
/// A PASS is excluded with the failures on purpose. Mail that really was sent
/// on the domain's authority and then DELIVERED is a contact form or a scanner
/// using the owner's address, and that is inbound mail with someone else's
/// words in it. The gate inherits that parser's documented residue, in the
/// OPPOSITE direction: NULL is that parser's cautious answer for its own
/// consumer and the trusting one here, so every case it declines to judge
/// (POP-fetched or imported mail, rows older than the column) reads as own.
/// On a provider it cannot read at all (every verdict NULL) this is the bare
/// address match the reader's `authored_by_account` (`messages.rs`) has
/// always been.
///
/// The COALESCE matters: a missing account row makes the comparison NULL,
/// which a `NOT` would read as "exclude" and a `bool` column read would reject.
///
/// A macro and not only a `const` so static SQL can `concat!` it, and declared
/// ahead of the modules so every one of them can.
macro_rules! own_mail_sql {
    () => {
        "(m.is_sent=1 OR COALESCE(m.auth_pass IS NULL AND trim(m.from_addr)!='' AND \
         lower(trim(m.from_addr))=(SELECT lower(trim(email)) FROM accounts WHERE id=m.account_id),0))"
    };
}
const OWN_MAIL: &str = own_mail_sql!();

/// SQL predicate for mail that is KNOWN to be kept close, over a `messages m`
/// row and its `triage t` row: what the product calls sealed. True when ANY
/// of these holds:
///
/// - the stored decision is AUTH mail (`auth.kinds` is not empty): a login
///   code, a reset link, a sign-in alert. The membership the Auth page reads
///   (`sealed_messages`).
/// - the stored decision RESTRICTS external access, or a person's correction
///   does, or the access assessment came back `restricted`. The refusing half
///   of what the agent door reads (`messages::external_messages_allowed_conn`).
/// - the retired `triage.sensitivity` column says so. NOTHING IN THE AGENT
///   PIPELINE WRITES IT ANY MORE (ingest always writes `normal`), so on its
///   own it guards only unmigrated legacy rows and rows a person sealed
///   through the compatibility endpoint. A guard that reads only this column
///   guards nothing arriving today.
///
/// "KNOWN" IS THE WORD THAT MATTERS. Mail whose access assessment is still
/// PENDING is not in here: nobody has said anything about it yet. Whether
/// that counts as kept close depends on who is asking, and there are exactly
/// two answers, so there are exactly two macros and the second is built from
/// this one ([`sealed_mail_sql!`]). Neither is ever written out by hand.
///
/// Correlated subqueries rather than joins, so it drops into any statement
/// that has `m` and `t` (including an `UPDATE triage AS t`); each is a
/// primary-key probe. A macro for the reason [`own_mail_sql!`] is.
///
/// The embedders' gate is built from this too ([`embeddable_mail_sql!`]).
macro_rules! known_sealed_mail_sql {
    () => {
        "(COALESCE(t.sensitivity,'normal')!='normal' \
          OR EXISTS(SELECT 1 FROM agent_message_decisions sealed_d \
                    WHERE sealed_d.account_id=m.account_id AND sealed_d.message_id=m.id \
                      AND (json_array_length(sealed_d.decision_json,'$.auth.kinds')>0 \
                           OR COALESCE(json_extract(sealed_d.decision_json,'$.external_access.restricted'),0))) \
          OR EXISTS(SELECT 1 FROM agent_message_state sealed_s \
                    WHERE sealed_s.account_id=m.account_id AND sealed_s.message_id=m.id \
                      AND sealed_s.access='restricted') \
          OR EXISTS(SELECT 1 FROM agent_triage_corrections sealed_c \
                    WHERE sealed_c.account_id=m.account_id AND sealed_c.message_id=m.id \
                      AND sealed_c.field='external_access' AND sealed_c.value_json='true'))"
    };
}

/// SQL predicate for mail a READER must leave out: [`known_sealed_mail_sql!`],
/// OR mail whose access assessment is still pending.
///
/// THE STRICT ANSWER, for everything that counts, scores or sweeps: the
/// per-sender Reading statistics, and smart done's scan and write. Unassessed
/// mail might be a login code, so it does not count for its sender and is not
/// swept until triage has said what it is. That costs nothing lasting: it
/// corrects itself the moment the decision commits.
///
/// THE ONE CALLER THAT USES THE LOOSER MACRO INSTEAD is the attention WRITE
/// (`record_message_attention`), and the asymmetry is deliberate. A report
/// refused is lost for good, and mail opened in its first moments, before
/// triage has reached it, is exactly when dwell is real. A report accepted
/// onto a row that later turns out to be a login code harms nothing, BECAUSE
/// every reader goes through this macro and drops that row once its decision
/// lands (`tests::smart_clear_guards::dwell_reported_before_an_auth_decision_
/// reaches_no_statistic`). Writers may be generous only as long as readers
/// are strict; a new reader of `dwell_ms` or `link_clicks` owes this macro.
macro_rules! sealed_mail_sql {
    () => {
        concat!(
            "(",
            known_sealed_mail_sql!(),
            " OR EXISTS(SELECT 1 FROM agent_message_state pending_s \
                        WHERE pending_s.account_id=m.account_id AND pending_s.message_id=m.id \
                          AND pending_s.access='pending'))"
        )
    };
}

/// SQL predicate for mail an EMBEDDER may read, over `messages m` and its
/// `triage t` row (LEFT JOINed is fine): the one gate on every vector write,
/// at ingest, in the drain, for a translation, and re-checked under the store
/// lock at the write itself (`search.rs`, `write_embedded_vectors`). Sealed
/// mail reaches neither the in-process embedder nor the shared embedding
/// service. True when ALL of these hold:
///
/// - not provider spam (semantic recall must never return it);
/// - not KNOWN sealed ([`known_sealed_mail_sql!`]: an auth decision, a
///   restriction by the model or a person, the retired column);
/// - and EITHER the account's own mail ([`own_mail_sql!`]), which is never
///   triaged by design (it is filed access-only) and is not a login code
///   somebody sent the owner, OR a stored decision exists and the access
///   assessment is not pending ([`sealed_mail_sql!`], the strict reader
///   answer, is false).
///
/// NOTHING IS EMBEDDED UNTIL IT IS KNOWN NOT TO BE SEALED. There is no local
/// login-code detector; only the triage model decides auth, and it runs after
/// ingest. So inbound mail whose triage is merely DELAYED (held by the daily
/// run cap, a spent budget, a failing model, no model configured at all) or
/// that is never triaged (an archive row nobody promoted) gets NO VECTOR
/// until a non-sealed decision lands. Keyword search still finds it: FTS is
/// not gated by this. That is the accepted trade-off. Own mail that a person
/// or the access check restricts loses its vector at that event
/// ([`vector_forbidden_sql!`]).
///
/// ORDER IS COST, not meaning: the drain asks this of every row with no
/// vector, and most of those (archive rows, mail awaiting triage) fail at the
/// decision probe, so that comes before the sealed probes. SQLite
/// short-circuits `AND`/`OR`.
macro_rules! embeddable_mail_sql {
    () => {
        concat!(
            "(m.is_spam=0 AND (",
            own_mail_sql!(),
            " OR (EXISTS(SELECT 1 FROM agent_message_decisions emb_d \
                         WHERE emb_d.account_id=m.account_id AND emb_d.message_id=m.id) \
                  AND NOT ",
            sealed_mail_sql!(),
            ")) AND NOT ",
            known_sealed_mail_sql!(),
            ")"
        )
    };
}

/// SQL predicate for a STORED VECTOR that must go, over `m` and `t`: the
/// message is known sealed, or it is in the agent pipeline (it has state)
/// and is not [`embeddable_mail_sql!`] (its content changed and awaits a new
/// decision, it moved to spam, it was embedded at ingest before this gate
/// existed and is still undecided). The cleanup at every event that can seal
/// a message and the sweep at store open both delete by it.
///
/// A row with NO agent state is a legacy row from before the agent pipeline:
/// the pipeline that wrote `triage.sensitivity` vetted it, so it keeps its
/// vector unless that column (or anything else) says sealed. The drain never
/// embeds such a row afresh: it has no decision.
macro_rules! vector_forbidden_sql {
    () => {
        concat!(
            "(",
            known_sealed_mail_sql!(),
            " OR (EXISTS(SELECT 1 FROM agent_message_state vf_s \
                         WHERE vf_s.account_id=m.account_id AND vf_s.message_id=m.id) \
                  AND NOT ",
            embeddable_mail_sql!(),
            "))"
        )
    };
}

/// Auth mail, over alias `d` (the message's `agent_message_decisions` row,
/// LEFT JOINed): its decision names an auth kind. The Rust form is
/// `AuthAssessment::is_auth`.
macro_rules! auth_mail_sql {
    () => {
        "(COALESCE(json_array_length(d.decision_json,'$.auth.kinds'),0)>0)"
    };
}

/// A takeover-class account change that holds FYE, over alias `d` and the
/// message alias `$m`: `critical` beside `account_change`, from a sender
/// that did not explicitly fail authentication (`auth_pass = 0`; NULL, never
/// evaluated, still counts). A spoof of the service's own address is not a
/// critical change. A look-alike domain passes for itself, which is the
/// prompt's to catch. The Rust form is `AuthAssessment::is_critical` plus
/// the same `auth_pass` test (`SealedMessage::critical` is this value).
macro_rules! auth_critical_sql {
    ($m:literal) => {
        concat!(
            "(json_extract(d.decision_json,'$.auth.critical')=1 AND EXISTS(SELECT 1 FROM \
               json_each(d.decision_json,'$.auth.kinds') WHERE value='account_change') \
             AND COALESCE(",
            $m,
            ".auth_pass,1)!=0)"
        )
    };
}

/// AN AUTH MESSAGE THAT HOLDS FYE, over aliases `d` (decision), `v`
/// (`auth_verdicts`, LEFT JOINed on the same message) and the message alias
/// `$m`: the owner said "Not me", or it is a critical account change
/// ([`auth_critical_sql!`]) they have not ruled on. A "That was me" lets go.
/// Only meaningful where `auth_mail_sql!` holds. The same predicate decides
/// FYE membership (through [`auth_fye_threads_cte!`]), spares a message from
/// thread-wide sweeps ([`holds_fye_triage_sql!`]) and keeps it from the
/// shredder.
macro_rules! auth_fye_sql {
    ($m:literal) => {
        concat!(
            "(COALESCE(v.verdict='not_mine',",
            auth_critical_sql!($m),
            ",0))"
        )
    };
}

/// The key a message's thread is grouped by: its thread id, or the message
/// alone when it has none.
macro_rules! thread_key_sql {
    ($m:literal) => {
        concat!(
            "(CASE WHEN ",
            $m,
            ".thread_id='' THEN '#'||",
            $m,
            ".id ELSE ",
            $m,
            ".thread_id END)"
        )
    };
}

/// An answered revisit: an `applied` run (`$r`, of `agent_triage_runs`) of a
/// `revisit:` job (`$j`, of `agent_triage_jobs`). The ghost-event revisit pass
/// reads the newest one per message as "already answered", and asks the
/// model again (a billed run) when it finds none; history retention keeps
/// that newest one for it (`retention.rs`). One definition, so the reader and
/// what is kept for it cannot drift apart.
macro_rules! revisit_answer_sql {
    ($r:literal, $j:literal) => {
        concat!(
            $r,
            ".outcome='applied' AND ",
            $j,
            ".id=",
            $r,
            ".job_id AND ",
            $j,
            ".account_id=",
            $r,
            ".account_id AND ",
            $j,
            ".trigger LIKE 'revisit:%'"
        )
    };
}

/// THE AUTH ARM OF FYE MEMBERSHIP, thread-level, as two CTEs to put after a
/// query's `WITH`: `auth_fye_threads(account_id, thread_key, id)` holds, per
/// thread, the newest not-done auth message that [`auth_fye_sql!`] holds for.
/// A thread qualifies when ANY of its auth messages does, so a critical
/// change is not lost behind the sign-in alert Gmail threads after it, and
/// that message is the one the FYE row shows. LEFT JOIN it on
/// `thread_key_sql!` of the outer message (or the attention row's thread).
///
/// COMPUTED ONCE PER QUERY, from the few rows that can qualify: decisions
/// flagged critical (`idx_decisions_critical`, a partial index on exactly
/// that expression, kept in step with `schema.sql`; the plan test
/// `auth_fye_threads_read_the_critical_index` fails if they drift) and "Not
/// me" verdicts. Never a per-row subquery: the
/// sitrep inventory and the stats run it over the whole window.
///
/// DRIVEN FROM THOSE FEW ROWS, PINNED: `auth_hold CROSS JOIN messages`, so
/// each held message is one rowid probe. Left to choose, a store with
/// statistics (every store, from the first pass a minute after the daemon
/// starts) ran it the other way round for as long as `auth_verdicts` is
/// empty, which on most stores is always: ANALYZE writes no row for an empty
/// table, the planner took the union for a large one, and it scanned every
/// message in the store, each probed into an automatic index built over the
/// union. That was 300 ms on a 50k store, in every read that carries this
/// CTE, where pinned it is a fraction of a millisecond. The window function
/// keeps the CTE from being flattened into an outer query, so the pin holds
/// in every reader. `auth_fye_threads_drive_from_the_held_messages` pins the
/// plan, with and without statistics.
///
/// NO REVISION CHECK: it reads the message's standing decision, so a
/// critical change or a "Not me" stays in FYE through a body heal or a
/// re-fetch (which bump the state revision) until the re-triage lands, the
/// way the sweeps and the shredder read it.
///
/// ONE DEFINITION for the FYE list, the sitrep's placement and Standing
/// band, and the stats tiers, so they cannot drift
/// (`auth_fye_list_and_placement_agree` pins that they do). The owner's
/// thread pin and a fired reminder still win, applied by each caller exactly
/// as for all FYE mail.
macro_rules! auth_fye_threads_cte {
    () => {
        concat!(
            "auth_hold AS (
               SELECT account_id, message_id FROM agent_message_decisions
                WHERE CASE WHEN json_valid(decision_json) THEN json_extract(decision_json,'$.auth.critical') END = 1
               UNION
               SELECT account_id, message_id FROM auth_verdicts WHERE verdict='not_mine'),
             auth_fye_threads AS (
               SELECT account_id, thread_key, id FROM (
                 SELECT fm.account_id, ",
            thread_key_sql!("fm"),
            " AS thread_key, fm.id,
                   ROW_NUMBER() OVER (PARTITION BY fm.account_id, ",
            thread_key_sql!("fm"),
            " ORDER BY fm.received_at DESC, fm.id DESC) AS pick
                 FROM auth_hold h
                 CROSS JOIN messages fm ON fm.account_id=h.account_id AND fm.id=h.message_id
                 JOIN agent_message_decisions d ON d.account_id=fm.account_id AND d.message_id=fm.id
                 LEFT JOIN auth_verdicts v ON v.account_id=fm.account_id AND v.message_id=fm.id
                 LEFT JOIN triage ft ON ft.account_id=fm.account_id AND ft.message_id=fm.id
                 WHERE fm.is_sent=0 AND fm.is_spam=0 AND fm.is_archive=0
                   AND COALESCE(ft.status,'new')!='done' AND ",
            auth_mail_sql!(),
            " AND ",
            auth_fye_sql!("fm"),
            ") WHERE pick=1)"
        )
    };
}

/// A message that holds its place against thread-wide sweeps, correlated to
/// the `triage` row being updated: [`auth_fye_sql!`], an auth message ruled
/// "Not me" or an unruled critical change. Done on ANOTHER message of the
/// thread (the Auth page archiving the sign-in alert beside it, a reminder,
/// an unsubscribe) must not clear it; a ruling, or done on the message
/// itself, does.
macro_rules! holds_fye_triage_sql {
    () => {
        concat!(
            "EXISTS(SELECT 1 FROM agent_message_decisions d \
               JOIN messages hm ON hm.account_id=d.account_id AND hm.id=d.message_id \
               LEFT JOIN auth_verdicts v ON v.account_id=d.account_id AND v.message_id=d.message_id \
             WHERE d.account_id=triage.account_id AND d.message_id=triage.message_id \
               AND CASE WHEN json_valid(d.decision_json) THEN ",
            auth_mail_sql!(),
            " AND ",
            auth_fye_sql!("hm"),
            " ELSE 0 END)"
        )
    };
}

/// Auth Page membership, over `m`, `t` (triage), `s` (agent_message_state)
/// and `d`: the agent classified it as auth, or it is an unmigrated legacy
/// row the old pipeline sealed. `triage.sensitivity` alone is never the
/// test: the agent pipeline no longer writes it.
macro_rules! sealed_membership_sql {
    () => {
        "(json_array_length(COALESCE(d.decision_json,'{}'),'$.auth.kinds')>0 \
          OR (s.message_id IS NULL AND t.sensitivity='sealed'))"
    };
}

/// The `auth_verdicts` join [`auth_fye_sql!`] reads, onto messages `m`.
macro_rules! auth_verdict_join_sql {
    () => {
        " LEFT JOIN auth_verdicts v ON v.account_id=m.account_id AND v.message_id=m.id "
    };
}

pub(crate) mod agent_budget;
/// An archive row nobody has assessed, over aliases `m` (messages) and `a`
/// (its `agent_message_state`, LEFT JOINed): stored to be found, never
/// checked. To an external reader such a row DOES NOT EXIST YET. It is left
/// out of every thread read, subject and count, and it does not close its
/// thread, because closing on it would shut every live conversation with a
/// year-old ancestor until somebody paid for a model call per ancestor.
///
/// Exactly this and nothing wider: a restricted archive row, a row the owner
/// sealed by hand, and any NON-archive pending row all still close the thread.
macro_rules! archive_unassessed_sql {
    () => {
        archive_unassessed_sql_as!("m", "a")
    };
}
/// [`archive_unassessed_sql!`] over any pair of aliases, so a subquery about a
/// SIBLING row can ask the same question in the same words. One definition:
/// the omission must not mean two things.
macro_rules! archive_unassessed_sql_as {
    ($m:literal, $a:literal) => {
        concat!(
            "(",
            $m,
            ".is_archive=1 AND (",
            $a,
            ".access IS NULL OR ",
            $a,
            ".access='pending')
           AND NOT EXISTS(SELECT 1 FROM agent_triage_corrections c
               WHERE c.account_id=",
            $m,
            ".account_id AND c.message_id=",
            $m,
            ".id
                 AND c.field='external_access' AND c.value_json='true'))"
        )
    };
}
/// An unassessed archive row an external caller may still BUY a check for:
/// not spam, and nothing queued, running or finished at its revision. That
/// leaves the rows never asked about and the ones whose check FAILED (which
/// `revive_failed_archive_access` puts back once the failure is a day old).
/// Same aliases as [`archive_unassessed_sql!`]. The search that picks ten
/// matches to ask about and the thread open that picks eight siblings both
/// use it, so a row already parked never takes a place a fresh one could.
macro_rules! archive_askable_sql {
    () => {
        concat!(
            "(m.is_spam=0 AND ",
            archive_unassessed_sql!(),
            " AND NOT EXISTS(SELECT 1 FROM agent_triage_jobs j
                 WHERE j.account_id=m.account_id AND j.message_id=m.id
                   AND j.kind IN ('triage','access') AND j.state!='failed'
                   AND j.input_revision=COALESCE(a.revision,1)))"
        )
    };
}
/// THE EXTERNAL SEARCH PREFILTER, over alias `m`: a cheap NECESSARY condition
/// for an external reader to be shown a row, written so a recall leg can carry
/// it in its own WHERE. IT AUTHORIZES NOTHING. The authority is
/// `messages::external_messages_allowed_conn` plus the thread guard
/// (`messages::thread_allowed_with_conn`), and every candidate that passes
/// this still goes through both before a field of it leaves the store.
///
/// What it is for: a row an external reader cannot be shown must not take a
/// place in a recall window, or which readable rows come back (and in what
/// order, and how fast) becomes a function of mail the caller may not read.
/// Three terms, each implied by the authority and none of them the whole of it:
/// 1. the row's own assessment is `allowed`;
/// 2. the owner has not sealed it;
/// 3. no sibling closes its thread on its face: a sibling the thread guard
///    counts (so not an unassessed archive row) that is itself not `allowed`
///    or is owner-sealed.
///
/// What it leaves to the authority: the recursive dependency check (a decision
/// that consumed a source since restricted, changed or deleted), on the row
/// and on every sibling. `the_external_prefilter_is_implied_by_the_authority`
/// in tests/archive.rs holds "necessary" over a fixture of every case; if the
/// authority ever loosens, that test is what says this must follow.
macro_rules! external_candidate_sql {
    () => {
        concat!(
            "(EXISTS(SELECT 1 FROM agent_message_state xa WHERE xa.account_id=m.account_id
                 AND xa.message_id=m.id AND xa.access='allowed')
              AND NOT EXISTS(SELECT 1 FROM agent_triage_corrections xc
                 WHERE xc.account_id=m.account_id AND xc.message_id=m.id
                   AND xc.field='external_access' AND xc.value_json='true')
              AND NOT EXISTS(SELECT 1 FROM messages s
                 LEFT JOIN agent_message_state sa
                   ON sa.account_id=s.account_id AND sa.message_id=s.id
                 WHERE s.account_id=m.account_id AND s.thread_id=m.thread_id AND s.id!=m.id
                   AND NOT ",
            archive_unassessed_sql_as!("s", "sa"),
            " AND (sa.access IS NULL OR sa.access!='allowed'
                     OR EXISTS(SELECT 1 FROM agent_triage_corrections sc
                         WHERE sc.account_id=s.account_id AND sc.message_id=s.id
                           AND sc.field='external_access' AND sc.value_json='true'))))"
        )
    };
}
/// [`external_candidate_sql!`] for a query that walks EVERY row of one account
/// (the external vector leg), where the correlated sibling scan of term 3 is
/// run once per row and is quadratic in thread length. The same three terms,
/// with the third turned inside out: the set of threads a sibling closes is
/// built once, for the account bound at `$account`, and a row passes when its
/// thread is not in it. Equal to the correlated form for any row that passes
/// terms 1 and 2, since such a row is never itself in the closing set;
/// `the_external_prefilter_is_implied_by_the_authority` holds the two equal
/// over its fixture.
macro_rules! external_candidate_flat_sql {
    ($account:literal) => {
        concat!(
            "(EXISTS(SELECT 1 FROM agent_message_state xa WHERE xa.account_id=m.account_id
                 AND xa.message_id=m.id AND xa.access='allowed')
              AND NOT EXISTS(SELECT 1 FROM agent_triage_corrections xc
                 WHERE xc.account_id=m.account_id AND xc.message_id=m.id
                   AND xc.field='external_access' AND xc.value_json='true')
              AND m.thread_id NOT IN (SELECT s.thread_id FROM messages s
                 LEFT JOIN agent_message_state sa
                   ON sa.account_id=s.account_id AND sa.message_id=s.id
                 WHERE s.account_id=",
            $account,
            " AND NOT ",
            archive_unassessed_sql_as!("s", "sa"),
            " AND (sa.access IS NULL OR sa.access!='allowed'
                     OR EXISTS(SELECT 1 FROM agent_triage_corrections sc
                         WHERE sc.account_id=s.account_id AND sc.message_id=s.id
                           AND sc.field='external_access' AND sc.value_json='true'))))"
        )
    };
}
mod agent_deliveries;
mod agent_triage;
mod attention;
mod audit;
mod calendar;
mod contacts;
pub mod device_tokens;
mod drafts;
mod events;
mod feedback;
mod ghosts;
pub mod groups;
mod messages;
mod migrate;
mod notify;
mod outbound;
pub mod people;
pub mod reading_senders;
mod renotify;
mod retention;
mod rules;
mod scheduled;
mod search;
mod senders;
pub mod smart_clear;
mod specialists;
mod sync_cursor;
#[cfg(test)]
mod tests;
mod tracking;
mod translations;
mod triage_stages;
mod zones;

pub use self::agent_triage::{
    ACCESS_BUDGET_KEY, ARCHIVE_ACCESS_BUDGET_KEY, ARCHIVE_ACCESS_OUTSTANDING_MAX,
    ARCHIVE_SURFACE_CAP, AccessProgress, ArchiveAccessCaps, BACKGROUND_BUDGET_KEY,
};
use self::migrate::migrate;
pub use self::retention::{HISTORY_RETENTION_DAYS, HistoryPruned};
pub use self::search::{EXTERNAL_RECALL_MAX, ExternalSearchMode};

use std::path::Path;
use std::sync::{Mutex, Once, RwLock};

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, params};

use crate::error::{CoreError, Result};
// The ledger's stored lane/decision strings ARE the metric labels; see `notify`.
use crate::metrics::{NotifyDecision, NotifyLane};
use crate::store::SmartBlockMemo;
use crate::store::{
    AccountIdentity, AccountPhoto, AgentDraftOutcome, AttachmentBytes, BankingApplied,
    BlankBodyScan, ClientActed, ClientPlatform, ClientSeen, ClientTag, ContactEntry, Device,
    DeviceToken, Draft, DraftFields, DraftKey, ExtractQueued, FtsQuery, HealScope, InboxUnread,
    IssuedDeviceToken, LeggedHit, MailActivityDay, MarketingApplied, MarketingOffer,
    MessageMissingRecipients, MessageOpen, MessageUnsub, MintedPairingCode, MissingVector,
    NewAuditEntry, NewEvent, NewNotifyDecision, NotifyDecisionRow, PersonNamed, SPAM_SYNCED_AT_KEY,
    SealedBody, SealedMessage, SearchDiagnostics, SearchFilter, SearchSort, SenderDigestRow,
    SenderDomainEntry, SenderEntry, SentMessage, Stage1Applied, Stage2CapOverrides, Stage2Usage,
    Stage2UsageDay, Store, SyncState, TermDf, TrackedMessage, TriageDebug, TriagedMessage,
    UsageTokens,
};
use crate::triage::rule_suggest::SmartBlock;
use crate::types::{
    AccountId, AttachmentInfo, AttentionStatus, AuditEntry, BandCounts, Banking, CalendarUpdate,
    ClientAttachment, ClientMessage, ClientThreadView, Deadline, Disposition, Event, EventKind,
    GroupHistoryEntry, GroupMember, GroupMode, NewMessage, OpenRate, Receipt, RetriageProgress,
    SanitizedMessage, SealedKind, SearchHit, SendGroup, SenderRule, Sensitivity, ShredCandidate,
    StoreStats, ThreadView, Tier, TriageAxis, TriageFeedback, UnsubscribeRecord,
};

/// `app_settings` key recording when this account's open ledger started, so the
/// share stat can refuse to look back further than the column has existed. See
/// `migrate::stamp_open_ledger_start`.
pub(crate) const OPEN_LEDGER_SINCE_KEY: &str = "opened_ledger_since";

/// `app_settings` key recording when this account's dwell ledger started: the
/// first open on which `triage.dwell_ms` existed. Dwell and click statistics
/// count only mail opened at or after it. See
/// `migrate::stamp_dwell_ledger_start`.
pub(crate) const DWELL_LEDGER_SINCE_KEY: &str = "dwell_ledger_since";

// schema.sql stays beside `store/mod.rs`; this file is `store/sqlite/mod.rs`.
const SCHEMA: &str = include_str!("../schema.sql");

/// The embedding dimension declared by the `message_vecs` vec0 table
/// (`FLOAT[384]`). The schema literal and this constant must move together;
/// attaching an embedder asserts they match.
pub const VEC_DIMS: usize = 384;

/// How long a connection waits out another process's write lock before giving
/// up. Generous next to the single transaction a `squelchd token`/`pair` command
/// runs, and short enough that a genuinely wedged writer still surfaces as an
/// error instead of a hang.
const BUSY_TIMEOUT_MS: u64 = 5_000;

/// Page cache per connection, in SQLite's negative-is-KiB spelling: 32 MB. See
/// [`SqliteStore::set_pragmas`].
const CACHE_SIZE_KIB_NEGATED: i64 = -32_000;

/// The WAL is truncated to this after a checkpoint: 64 MB. See
/// [`SqliteStore::set_pragmas`].
const JOURNAL_SIZE_LIMIT_BYTES: i64 = 64 * 1024 * 1024;

/// Compiled statements the store's connection keeps for `prepare_cached`.
/// rusqlite's default is 16, and the store has more than twice that many
/// cached statements (the job claim, the message reads, the listings, the
/// access walks, ...), so at 16 they would evict each other. A compiled
/// statement here is 8 to 40 KB (`StatementStatus::MemUsed`), so a full cache
/// is one to three megabytes.
///
/// WHAT THE CACHE DOES NOT DO: stop the recompiling. SQLite marks a statement
/// whose plan looked at a bound value (an integer `col=?`, a `LIMIT ?`) as
/// expired when it is bound again, and rusqlite clears the bindings when it
/// takes a statement back, so a cached one is still compiled once per call.
/// An uncached one was compiled twice (at prepare, and again after binding),
/// which is what caching saves: the job claim 207 us to 126 us. Ending the
/// recompile takes the query planner stability guarantee
/// (`SQLITE_DBCONFIG_ENABLE_QPSG`), which changes how every statement is
/// planned and is measured and argued separately.
const STATEMENT_CACHE_CAPACITY: usize = 64;

/// Rows `ANALYZE` samples per index (`PRAGMA analysis_limit`). LARGE, so a
/// table under it is scanned in full and the statistics say what the data
/// says: at the 400 SQLite's docs suggest for a cheap pass, the sampler
/// visits ~400 rows and scales, and on a single-account store that records
/// `account_id = ?` as matching ~400 rows of 50,000, which is "selective",
/// the exact belief this pass exists to correct. A table past the limit is
/// still bounded.
///
/// NEVER ZERO. Zero is also the switch that makes STAT4 (compiled in) persist
/// real index-key samples, sender addresses and subjects among them, into the
/// file, where they would outlive the rows the shredder removes. See
/// [`SqliteStore::refresh_statistics`].
const ANALYSIS_LIMIT_ROWS: i64 = 1_000_000;

/// The `PRAGMA optimize` mask the statistics pass runs with: SQLite's default
/// (`0xfffe`) minus bit `0x10`, which would cap every ANALYZE the pragma
/// issues at the lesser of the connection's `analysis_limit` and 2,000 rows.
/// Under that cap a table that grew 10x would be re-analyzed as a 2,000-row
/// sample, which is the very "`account_id = ?` matches 2,000 of N" belief
/// [`ANALYSIS_LIMIT_ROWS`] exists to keep out of the file. Without the bit
/// the pragma's ANALYZE runs under the connection's limit, like step 1.
const OPTIMIZE_MASK: u32 = 0xfffe & !0x10;

static VEC_EXT_INIT: Once = Once::new();

/// Register the statically-linked sqlite-vec (`vec0`) extension on SQLite's
/// auto-extension hook, so every later connection has the virtual table. Runs
/// once per process and MUST run before the schema, which creates
/// `message_vecs USING vec0(...)`.
fn register_vec_extension() {
    VEC_EXT_INIT.call_once(|| {
        // SAFETY: `sqlite3_vec_init` is the C entrypoint sqlite-vec statically
        // links; transmuting it to the auto-extension fn pointer type is the
        // documented rusqlite integration pattern, spelled out explicitly here
        // for clippy::missing_transmute_annotations.
        unsafe {
            let init: unsafe extern "C" fn(
                *mut rusqlite::ffi::sqlite3,
                *mut *mut std::os::raw::c_char,
                *const rusqlite::ffi::sqlite3_api_routines,
            ) -> std::os::raw::c_int =
                std::mem::transmute(sqlite_vec::sqlite3_vec_init as *const ());
            rusqlite::ffi::sqlite3_auto_extension(Some(init));
        }
    });
}

pub struct SqliteStore {
    conn: Mutex<Connection>,
    /// The on-box embedder: embeds query text for the vector searches and message
    /// bodies for callers. `None` when semantic recall is not wired — the vector
    /// methods then return [`CoreError::InvalidInput`] and hybrid search degrades
    /// to keyword-only. `RwLock` because it can be attached LATE, while the store
    /// is already shared behind an `Arc`.
    embedder: RwLock<Option<std::sync::Arc<dyn crate::embed::Embedder>>>,
    /// In-process wake signal for newly-appended `events` rows, attachable late
    /// like `embedder`.
    ///
    /// THE PAYLOAD IS ONLY A HINT — the `events` TABLE is the source of truth. A
    /// reader wakes on any id and re-reads every row past its own cursor, so a
    /// lagged or missed broadcast costs only latency, which is why send errors
    /// are ignored.
    event_tx: RwLock<Option<tokio::sync::broadcast::Sender<i64>>>,
    /// The inventory half of the last `stats()`, and the [`WriteToken`] it was
    /// read at, so a poll that finds nothing written since reuses it
    /// (`attention::InventoryMemo`).
    inventory_memo: Mutex<Option<attention::InventoryMemo>>,
}

/// Whether anything has been written to the file since a read: this
/// connection's count of rows inserted, updated or deleted
/// (`sqlite3_total_changes`, every statement including triggers and those
/// rolled back) and `PRAGMA data_version`, which moves when ANOTHER
/// connection commits (`squelchd token`, `pair`, a standalone `squelch-mcp`).
/// Equal tokens mean the same data. Unequal ones may not (a rolled-back
/// write, an update to the same value), which only costs a recompute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct WriteToken {
    changes: u64,
    data_version: i64,
}

impl WriteToken {
    pub(super) fn of(conn: &Connection) -> Result<Self> {
        Ok(Self {
            changes: conn.total_changes(),
            data_version: conn
                .prepare_cached("PRAGMA data_version")?
                .query_row([], |r| r.get(0))?,
        })
    }
}

impl SqliteStore {
    /// Overwrite the account's creation stamp with `raw`, for a test that
    /// wants the store to stop answering [`Store::account_created_at`].
    #[cfg(test)]
    pub(crate) fn test_set_account_created_at(&self, account_id: AccountId, raw: &str) {
        let conn = self.lock().unwrap();
        conn.execute(
            "UPDATE accounts SET created_at = ?2 WHERE id = ?1",
            params![account_id, raw],
        )
        .unwrap();
    }

    /// Open (or create) a store at `path`, applying the schema.
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        register_vec_extension();
        let conn = Connection::open(path)?;
        Self::init(conn)
    }

    /// Open an in-memory store (tests).
    pub fn open_in_memory() -> Result<Self> {
        register_vec_extension();
        let conn = Connection::open_in_memory()?;
        Self::init(conn)
    }

    /// Put a fresh connection into the mode a SECOND PROCESS can share.
    ///
    /// `squelchd token issue`, `token revoke` and `pair` open this same file
    /// while `squelchd serve` holds it, so the store has real multi-process
    /// writers now, not just the daemon's one mutex:
    ///
    /// - WAL lets the CLI write while the daemon reads, instead of the rollback
    ///   journal's whole-file lock turning every mint into `database is locked`;
    /// - `busy_timeout` makes the two writers QUEUE for the few milliseconds a
    ///   token transaction takes rather than fail instantly on contention.
    ///
    /// Run on every open: `busy_timeout` is per-connection, and `journal_mode` is
    /// a persistent property of the FILE, so whichever process opens it first
    /// converts it and the rest simply confirm. The result is read and discarded
    /// because `:memory:` answers "memory" and there is nothing to assert there
    /// (the file-backed case is asserted in the store tests).
    ///
    /// A failure PROPAGATES rather than being swallowed. The conversion needs the
    /// file to itself, so it can lose to a process still holding the old mode
    /// open, and a store that quietly stayed on the rollback journal would
    /// reproduce the locking it is here to prevent with nothing to point at.
    ///
    /// Two more are per-connection tuning, so they too run on every open:
    ///
    /// - `cache_size=-32000` is 32 MB of page cache instead of the 2 MB default,
    ///   against a file that runs 200 MB to 1 GB on a real mailbox. It is a
    ///   ceiling, not a reservation: pages are cached as they are read, and an
    ///   idle connection holds only what it has touched.
    /// - `journal_size_limit` truncates the WAL back to 64 MB after a
    ///   checkpoint. Without it the file keeps the high-water mark of the
    ///   largest transaction ever run (the whole-mailbox FTS rebuild) for the
    ///   life of the database.
    ///
    /// Two that look like they belong here are left at SQLite's defaults ON
    /// PURPOSE:
    ///
    /// - `synchronous` stays FULL. NORMAL is the usual WAL pairing and would
    ///   save an fsync per commit, but this daemon makes irreversible moves on
    ///   the strength of its own commits: the scheduled-send claim must be on
    ///   disk before the Gmail POST (`scheduled_send.rs` re-fires a row that
    ///   reads `scheduled` after a crash, and only searches for the sent copy
    ///   when `maybe_sent` says it may exist), and the notify ledger is the
    ///   dedupe for a push already delivered and a model call already billed.
    ///   Under NORMAL a power loss inside the OS writeback window could roll
    ///   either back after the side effect: a mail sent twice. The right shape
    ///   is NORMAL with those claims synced explicitly, which is a design
    ///   change, not a pragma.
    /// - `temp_store` stays FILE. MEMORY would keep the planner's temp b-trees
    ///   off disk, but in this SQLite it also disables the sorter's spill, so a
    ///   `CREATE INDEX` over a large table inside `migrate` (the one place a
    ///   big sort is certain) would hold every key in heap: on a memory-capped
    ///   tenant pod that is an OOM kill during open, on every restart.
    ///
    /// Not a pragma but set here with them: the statement cache's capacity,
    /// [`STATEMENT_CACHE_CAPACITY`]. `foreign_keys=ON`, the one other
    /// per-connection setting, is applied by `schema.sql`'s first line through
    /// `execute_batch(SCHEMA)` in `init`.
    fn set_pragmas(conn: &Connection) -> Result<()> {
        conn.busy_timeout(std::time::Duration::from_millis(BUSY_TIMEOUT_MS))?;
        let _: String = conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))?;
        conn.pragma_update(None, "cache_size", CACHE_SIZE_KIB_NEGATED)?;
        conn.pragma_update(None, "journal_size_limit", JOURNAL_SIZE_LIMIT_BYTES)?;
        // Not a pragma, but per connection like them.
        conn.set_prepared_statement_cache_capacity(STATEMENT_CACHE_CAPACITY);
        Ok(())
    }

    fn init(conn: Connection) -> Result<Self> {
        Self::set_pragmas(&conn)?;
        conn.execute_batch(SCHEMA)?;
        // SCHEMA is all `CREATE TABLE IF NOT EXISTS`, so a pre-existing DB never
        // picks up freshly-added columns from the CREATE — `migrate` adds them.
        // New tables/indexes need no migration (IF NOT EXISTS covers them),
        // EXCEPT an index naming a migrated column, which lives in `migrate`.
        migrate(&conn)?;
        // THE BACKLOG SWEEP, once per open: vectors written before the sealed
        // gate existed (login codes embedded at ingest), and any an event
        // missed, go before anything can search them. One transaction, so a
        // backlog of hundreds pays one commit, not two per vector.
        {
            let tx = conn.unchecked_transaction()?;
            search::purge_forbidden_vectors_conn(&tx, None)?;
            tx.commit()?;
        }
        Ok(Self {
            conn: Mutex::new(conn),
            embedder: RwLock::new(None),
            event_tx: RwLock::new(None),
            inventory_memo: Mutex::new(None),
        })
    }

    /// Give the query planner statistics to plan with.
    ///
    /// Until this existed NOTHING in the daemon ever ran `ANALYZE`, so the
    /// planner had never seen a row count for the file. Its default guess is
    /// that an equality on any indexed column is selective, which on a store
    /// where every row carries the same `account_id` sends it down the
    /// `account_id` prefix of whatever index it finds and walks the whole
    /// account: the recursive external-access check measured 457 ms at 50k
    /// messages with no statistics and 0.2 ms with them, `agent_sender_history`
    /// 64 ms to 0.0, and several statements carry `INDEXED BY` pins only
    /// because the planner could not be trusted to find the index on its own.
    ///
    /// Two steps, every call:
    ///
    /// 1. `ANALYZE` each REAL table that has no statistics yet. "Real" leaves
    ///    out the FTS5 and vec0 shadow tables (`messages_fts_data`,
    ///    `message_vecs_chunks`, ...): they hold the bodies and the vectors,
    ///    the largest b-trees in the file, nothing plans against them, and an
    ///    unqualified `ANALYZE` would walk every one of them end to end,
    ///    because the per-index row count is exact whatever the sampling
    ///    limit says.
    /// 2. `PRAGMA optimize` under [`OPTIMIZE_MASK`], which in this SQLite
    ///    (3.46) re-analyzes a table the connection has planned against
    ///    whose size has moved 10x since its statistics were taken, or that
    ///    has an index with none, and is a no-op otherwise. That second rule
    ///    does reach the small keyed shadow tables FTS5 and vec0 look rows
    ///    up in (`messages_fts_idx`, `messages_fts_config`,
    ///    `message_vecs_info`): a few rows each, taken once, SQLite's own
    ///    call. The blob tables have no index for it to want statistics on,
    ///    and their growth is only checked against the connection's own
    ///    queries, which never touch them, so it never walks them.
    ///
    /// NOT on the open path, on purpose. Step 1 on a cold multi-hundred-MB
    /// file is seconds of b-tree walking, which belongs after the daemon's
    /// door is bound rather than before, and never on the short CLI opens
    /// (`squelchd token`, `squelchd pair`) that share the file with a serving
    /// daemon: both steps take the write lock, and an open that waited out
    /// `busy_timeout` behind the sync engine would have logged a failure for
    /// nothing. The daemon calls this on a schedule instead
    /// (`spawn_statistics_refresh` in squelchd).
    ///
    /// Under [`ANALYSIS_LIMIT_ROWS`] throughout; see that constant for why
    /// the limit is large and why it must never be zero.
    pub fn refresh_statistics(&self) -> Result<()> {
        let conn = self.lock()?;
        conn.pragma_update(None, "analysis_limit", ANALYSIS_LIMIT_ROWS)?;
        let real_tables: Vec<String> = {
            let mut stmt = conn.prepare(
                "SELECT name FROM sqlite_master
                 WHERE type = 'table'
                   AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\'
                   AND name NOT LIKE 'messages\\_fts%' ESCAPE '\\'
                   AND name NOT LIKE 'message\\_vecs%' ESCAPE '\\'",
            )?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            rows.collect::<std::result::Result<_, _>>()?
        };
        // `sqlite_stat1` does not exist until the first ANALYZE, and a SELECT
        // naming it does not compile on such a file, so ask `sqlite_master`
        // before asking it.
        let stat_table: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='sqlite_stat1')",
            [],
            |r| r.get(0),
        )?;
        let analyzed: std::collections::HashSet<String> = if stat_table {
            let mut stmt = conn.prepare("SELECT DISTINCT tbl FROM sqlite_stat1")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            rows.collect::<std::result::Result<_, _>>()?
        } else {
            std::collections::HashSet::new()
        };
        for table in real_tables.iter().filter(|t| !analyzed.contains(*t)) {
            // Quoted: these are our own names from schema.sql, but a name is
            // a name.
            conn.execute_batch(&format!("ANALYZE \"{}\"", table.replace('"', "\"\"")))?;
        }
        conn.execute_batch(&format!("PRAGMA optimize({OPTIMIZE_MASK:#x})"))?;
        Ok(())
    }

    /// Attach an [`Embedder`](crate::embed::Embedder) so the semantic-recall
    /// methods work. Fails loudly on a dimensionality mismatch with [`VEC_DIMS`],
    /// which would otherwise silently corrupt the index.
    pub fn with_embedder(
        self,
        embedder: std::sync::Arc<dyn crate::embed::Embedder>,
    ) -> Result<Self> {
        self.attach_embedder(embedder)?;
        Ok(self)
    }

    /// Swap in the embedder while the store may ALREADY be shared behind an `Arc`
    /// (`&self`): `squelchd serve` binds its HTTP port first and attaches in the
    /// background, so search runs keyword-only until this fires. Fails loudly on
    /// a [`VEC_DIMS`] mismatch. Returns the previous embedder.
    pub fn attach_embedder(
        &self,
        embedder: std::sync::Arc<dyn crate::embed::Embedder>,
    ) -> Result<Option<std::sync::Arc<dyn crate::embed::Embedder>>> {
        if embedder.dims() != VEC_DIMS {
            return Err(CoreError::InvalidInput(format!(
                "embedder dims {} != message_vecs vec0 width {VEC_DIMS}",
                embedder.dims()
            )));
        }
        let mut guard = self
            .embedder
            .write()
            .map_err(|_| CoreError::Other(anyhow::anyhow!("embedder lock poisoned")))?;
        Ok(guard.replace(embedder))
    }

    /// The attached embedder, if any — a cheap `Arc` clone.
    pub fn embedder(&self) -> Option<std::sync::Arc<dyn crate::embed::Embedder>> {
        self.embedder.read().ok().and_then(|g| g.clone())
    }

    /// Attach the in-process broadcast that every successful
    /// [`Store::append_event`] pokes with the new event id, late-attachable like
    /// [`SqliteStore::attach_embedder`]. Returns the previous sender.
    ///
    /// OPTIONAL and persists nothing: the `events` table is the source of truth,
    /// so a consumer that never attaches simply polls instead.
    pub fn attach_event_notifier(
        &self,
        tx: tokio::sync::broadcast::Sender<i64>,
    ) -> Result<Option<tokio::sync::broadcast::Sender<i64>>> {
        let mut guard = self
            .event_tx
            .write()
            .map_err(|_| CoreError::Other(anyhow::anyhow!("event notifier lock poisoned")))?;
        Ok(guard.replace(tx))
    }

    /// The attached event notifier, if any. Cheap clone of the sender.
    pub fn event_notifier(&self) -> Option<tokio::sync::broadcast::Sender<i64>> {
        self.event_tx.read().ok().and_then(|g| g.clone())
    }

    /// Run raw SQL on the store's connection. For a test outside this module
    /// that has to break the store on purpose (a trigger that refuses one
    /// row); nothing in the daemon has a use for it.
    #[cfg(test)]
    pub(crate) fn execute_batch_for_test(&self, sql: &str) {
        self.lock().unwrap().execute_batch(sql).unwrap();
    }

    /// One integer off the store's connection, for a test outside this
    /// module that has to read a row no store method returns.
    #[cfg(test)]
    pub(crate) fn query_i64_for_test(&self, sql: &str) -> Option<i64> {
        use rusqlite::OptionalExtension;
        self.lock()
            .unwrap()
            .query_row(sql, [], |r| r.get(0))
            .optional()
            .unwrap()
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>> {
        self.conn
            .lock()
            .map_err(|_| CoreError::Other(anyhow::anyhow!("store mutex poisoned")))
    }

    /// Convenience for tests/other crates: create an account, return its id.
    /// Also seeds the one contact row the mail path can never produce — the
    /// account's own address (see [`SqliteStore::seed_self_contact_conn`]).
    pub fn ensure_account(&self, email: &str) -> Result<AccountId> {
        let now = Utc::now().to_rfc3339();
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO accounts(email, created_at) VALUES(?1, ?2)
             ON CONFLICT(email) DO NOTHING",
            params![email, now],
        )?;
        let id: i64 = conn.query_row(
            "SELECT id FROM accounts WHERE email = ?1",
            params![email],
            |r| r.get(0),
        )?;
        Self::seed_self_contact_conn(&conn, id, email, &now)?;
        Ok(id)
    }

    /// The user is a contact of the user. Nothing in the mail path will ever say
    /// so: the per-message Sent seed and the Sent-history harvest both STRIP the
    /// account's own address from the recipients they record, deliberately — a
    /// CC to yourself is not evidence you know yourself. So without this row a
    /// note the user mails to themselves arrives from a total stranger: Stage-1
    /// misses its known-contact rung, neither LLM pass gets the importance
    /// floor, the standing band's correspondence arm does not match, and the
    /// composer cannot autocomplete the user's own address.
    ///
    /// ONE ROW, seeded here, because all four of those tests are the same
    /// `contacts` lookup — the two LLM queues and the standing band ask it in
    /// SQL, `is_known_contact` and `search_contacts` ask it in Rust. A self case
    /// written into each of them instead is four chances to drift apart.
    ///
    /// This runs on every daemon start, which is also what backfills an account
    /// created before the row existed. DO NOTHING on conflict rather than an
    /// upsert, precisely because it re-runs: a later start must not stomp a
    /// count or a name that has since moved. `sent_count = 1` satisfies the
    /// `sent_count > 0` every known-contact predicate applies, and the display
    /// name is what makes typing "me" offer your own address.
    fn seed_self_contact_conn(
        conn: &Connection,
        account_id: AccountId,
        email: &str,
        now: &str,
    ) -> Result<()> {
        let addr = email.trim().to_ascii_lowercase();
        if addr.is_empty() {
            return Ok(());
        }
        conn.execute(
            "INSERT INTO contacts(account_id, addr, sent_count, first_seen,
                                  last_sent_at, display_name)
             VALUES(?1,?2,1,?3,?3,'Me')
             ON CONFLICT(account_id, addr) DO NOTHING",
            params![account_id, addr, now],
        )?;
        Ok(())
    }

    /// The account's own email address. [`CoreError::NotFound`] for an unknown
    /// id, like every other single-row getter here.
    pub fn account_email(&self, account_id: AccountId) -> Result<String> {
        let conn = self.lock()?;
        let email: Option<String> = conn
            .query_row(
                "SELECT email FROM accounts WHERE id = ?1",
                params![account_id],
                |r| r.get(0),
            )
            .optional()?;
        email.ok_or(CoreError::NotFound)
    }

    /// HUMAN-DOOR ACTION SUPPORT (squelch-api only): resolve a local message id
    /// to the Gmail ids + headers an action needs (archive/label/send).
    ///
    /// UNFILTERED by sensitivity: a sealed or access-restricted message resolves
    /// like any other; only an unknown id or another account's id is NotFound.
    /// This is deliberate, not a hole. Human-door actions do not treat a legacy
    /// `sensitivity = 'sealed'` as a denial (docs/SECURITY.md §4); what gates
    /// them is the confirm flag and audit ladder in squelch-api's
    /// `guarded_action`. The access authority for the agent door is
    /// `messages.rs:external_messages_allowed_conn` (via `thread_allowed_with_conn`
    /// / [`Self::external_thread_allowed`]), which this lookup does NOT apply, so
    /// never hand its result to an agent-facing path.
    pub fn action_message_ref(
        &self,
        account_id: AccountId,
        message_id: i64,
    ) -> Result<crate::store::ActionMessageRef> {
        let conn = self.lock()?;
        let row = conn
            .query_row(
                "SELECT m.id, m.gmail_msg_id, m.thread_id, m.from_addr, m.from_name, m.subject
                 FROM messages m
                 WHERE m.account_id = ?1 AND m.id = ?2",
                params![account_id, message_id],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, Option<String>>(4)?,
                        r.get::<_, String>(5)?,
                    ))
                },
            )
            .optional()?;
        let (id, gmail_msg_id, thread_id, from_addr, from_name, subject) =
            row.ok_or(CoreError::NotFound)?;
        Ok(crate::store::ActionMessageRef {
            id,
            account_id,
            gmail_msg_id,
            thread_id,
            from_addr,
            from_name,
            subject,
        })
    }

    /// Test/local helper: write a triage row for a message. Real triage is
    /// written by the triage pipeline; this keeps the store self-contained.
    #[allow(clippy::too_many_arguments)]
    pub fn set_triage(
        &self,
        message_id: i64,
        account_id: AccountId,
        importance: u8,
        tier: Tier,
        sensitivity: crate::types::Sensitivity,
        sealed_kind: Option<crate::types::SealedKind>,
        one_line: &str,
        reason: &str,
        deadline: Option<DateTime<Utc>>,
    ) -> Result<()> {
        let conn = self.lock()?;
        // Rows created here are treated as Stage-1-finished and escalated
        // (`stage1_model_used='rule'`, `needs_stage2=1`) so they land in the
        // Stage-2 queue.
        conn.execute(
            "INSERT INTO triage(message_id, account_id, importance, tier, sensitivity,
                 sealed_kind, one_line, reason, deadline,
                 stage1_model_used, needs_stage2, created_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,'rule',1,?10)
             ON CONFLICT(message_id) DO UPDATE SET
                 importance=excluded.importance, tier=excluded.tier,
                 sensitivity=excluded.sensitivity, sealed_kind=excluded.sealed_kind,
                 one_line=excluded.one_line, reason=excluded.reason,
                 deadline=excluded.deadline",
            params![
                message_id,
                account_id,
                importance as i64,
                tier.as_str(),
                sensitivity.as_str(),
                sealed_kind.map(|k| k.as_str()),
                one_line,
                reason,
                deadline.map(|d| d.to_rfc3339()),
                Utc::now().to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    /// Test/local helper: set the per-property `field_reasons` JSON blob on an
    /// existing triage row. Real reasons are written by the triage pipeline;
    /// this lets tests seed the human-door insight column directly.
    pub fn set_field_reasons(
        &self,
        message_id: i64,
        account_id: AccountId,
        field_reasons: &crate::types::FieldReasons,
    ) -> Result<()> {
        let json = serde_json::to_string(field_reasons).ok();
        let conn = self.lock()?;
        conn.execute(
            "UPDATE triage SET field_reasons = ?3
             WHERE message_id = ?1 AND account_id = ?2",
            params![message_id, account_id, json],
        )?;
        Ok(())
    }

    /// Test/local helper: insert one attachment row directly (real ones come from
    /// [`Store::ingest_message`]), so tests can seed the byte-serving endpoint —
    /// including an over-cap `data == None` row — without an RFC822 fixture.
    /// Lands a NULL `content_id`, the shape of a real (non-inline) attachment;
    /// seed a cid-inline part with [`Self::insert_inline_attachment`].
    pub fn insert_attachment(
        &self,
        account_id: AccountId,
        message_id: i64,
        filename: &str,
        mime: &str,
        size_bytes: i64,
        data: Option<&[u8]>,
    ) -> Result<i64> {
        self.insert_inline_attachment(
            account_id, message_id, filename, mime, size_bytes, data, None,
        )
    }

    /// [`Self::insert_attachment`] plus the part's normalized `content_id` — the
    /// seam a test needs to stand up a cid-inline image without an RFC822 fixture.
    #[allow(clippy::too_many_arguments)] // the columns of one attachment row
    pub fn insert_inline_attachment(
        &self,
        account_id: AccountId,
        message_id: i64,
        filename: &str,
        mime: &str,
        size_bytes: i64,
        data: Option<&[u8]>,
        content_id: Option<&str>,
    ) -> Result<i64> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO attachments(account_id, message_id, filename, mime, size_bytes, data, content_id)
             VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![
                account_id,
                message_id,
                filename,
                mime,
                size_bytes,
                data,
                content_id
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }
}

/// Shared by every submodule: a stored RFC3339 timestamp back into a `DateTime`.
fn parse_dt(s: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&Utc))
        .map_err(|e| CoreError::InvalidInput(format!("bad datetime {s:?}: {e}")))
}

/// Read column `i` as a stored RFC3339 timestamp INSIDE a rusqlite mapper, so a
/// list method can build its struct in one pass instead of yielding a tuple and
/// parsing after. A mapper cannot fail with a [`CoreError`], so a malformed
/// stored timestamp surfaces as `CoreError::Sqlite` rather than the
/// `CoreError::InvalidInput` [`parse_dt`] gives; both abort the same read.
fn dt(r: &rusqlite::Row<'_>, i: usize) -> rusqlite::Result<DateTime<Utc>> {
    let s: String = r.get(i)?;
    parse_col_dt(i, &s)
}

/// [`dt`] for a nullable column: SQL NULL is `None`, a present value must parse.
fn dt_opt(r: &rusqlite::Row<'_>, i: usize) -> rusqlite::Result<Option<DateTime<Utc>>> {
    match r.get::<_, Option<String>>(i)? {
        Some(s) => parse_col_dt(i, &s).map(Some),
        None => Ok(None),
    }
}

/// A COUNT or SUM column as `u64`. rusqlite 0.41 no longer reads `u64`
/// directly (SQLite integers are signed), so read `i64` and refuse a negative
/// rather than let it wrap.
fn count(r: &rusqlite::Row<'_>, i: usize) -> rusqlite::Result<u64> {
    let n: i64 = r.get(i)?;
    u64::try_from(n).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(i, rusqlite::types::Type::Integer, Box::new(e))
    })
}

fn parse_col_dt(i: usize, s: &str) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&Utc))
        .map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(i, rusqlite::types::Type::Text, Box::new(e))
        })
}

/// Every method here is a one-line delegation to the inherent method of the
/// same name, which lives in the subject module named above. A trait impl
/// cannot be split across files, so the bodies moved and this block stayed.
impl Store for SqliteStore {
    fn upsert_message(&self, msg: &NewMessage) -> Result<i64> {
        self.upsert_message(msg)
    }

    fn thread_view(&self, account_id: AccountId, thread_id: &str) -> Result<ThreadView> {
        self.thread_view(account_id, thread_id)
    }

    fn thread_id_for_message(
        &self,
        account_id: AccountId,
        message_id: i64,
    ) -> Result<Option<String>> {
        self.thread_id_for_message(account_id, message_id)
    }

    fn thread_view_with_html(
        &self,
        account_id: AccountId,
        thread_id: &str,
    ) -> Result<ClientThreadView> {
        self.thread_view_with_html(account_id, thread_id)
    }

    fn attachment_bytes(
        &self,
        account_id: AccountId,
        attachment_id: i64,
    ) -> Result<Option<AttachmentBytes>> {
        self.attachment_bytes(account_id, attachment_id)
    }

    fn deadlines(&self, account_id: AccountId, within_days: Option<u32>) -> Result<Vec<Deadline>> {
        self.deadlines(account_id, within_days)
    }

    fn upsert_shipment(
        &self,
        account_id: AccountId,
        message_id: i64,
        shipment: &crate::triage::ShipmentInfo,
        seen_at: DateTime<Utc>,
    ) -> Result<i64> {
        self.upsert_shipment(account_id, message_id, shipment, seen_at)
    }

    fn list_shipments(
        &self,
        account_id: AccountId,
        include_delivered: bool,
        policy: crate::config::ShipmentListPolicy,
    ) -> Result<Vec<crate::types::Shipment>> {
        self.list_shipments(account_id, include_delivered, policy)
    }

    fn clear_shipment(
        &self,
        account_id: AccountId,
        shipment_id: i64,
        at: DateTime<Utc>,
        policy: crate::config::ShipmentListPolicy,
    ) -> Result<bool> {
        self.clear_shipment(account_id, shipment_id, at, policy)
    }

    fn list_ghost_events(
        &self,
        account_id: AccountId,
        from: DateTime<chrono::FixedOffset>,
        to: DateTime<chrono::FixedOffset>,
    ) -> Result<Vec<crate::types::GhostEvent>> {
        self.list_ghost_events(account_id, from, to)
    }

    fn patch_ghost_user(
        &self,
        account_id: AccountId,
        ghost_id: i64,
        end: Option<Option<crate::triage::ghost::EventTime>>,
        state: Option<Option<crate::types::GhostUserState>>,
        at: DateTime<Utc>,
    ) -> Result<bool> {
        self.patch_ghost_user(account_id, ghost_id, end, state, at)
    }

    fn shipments_redetect_cleanup(&self, account_id: AccountId) -> Result<u64> {
        self.shipments_redetect_cleanup(account_id)
    }

    fn receipts_reparse_cleanup(&self, account_id: AccountId) -> Result<u64> {
        self.receipts_reparse_cleanup(account_id)
    }

    fn list_pollable_shipments(
        &self,
        account_id: AccountId,
        min_first_seen: DateTime<Utc>,
        max_failures: u32,
    ) -> Result<Vec<crate::types::Shipment>> {
        self.list_pollable_shipments(account_id, min_first_seen, max_failures)
    }

    fn apply_carrier_track(
        &self,
        account_id: AccountId,
        shipment_id: i64,
        track: &crate::triage::CarrierTrack,
        polled_at: DateTime<Utc>,
    ) -> Result<bool> {
        self.apply_carrier_track(account_id, shipment_id, track, polled_at)
    }

    fn record_poll_outcome(
        &self,
        account_id: AccountId,
        shipment_id: i64,
        polled_at: DateTime<Utc>,
        permanent_failure: bool,
    ) -> Result<()> {
        self.record_poll_outcome(account_id, shipment_id, polled_at, permanent_failure)
    }

    fn upsert_receipt(
        &self,
        account_id: AccountId,
        message_id: i64,
        from_addr: &str,
        from_name: Option<&str>,
        receipt: &crate::triage::ReceiptInfo,
        received_at: DateTime<Utc>,
    ) -> Result<i64> {
        self.upsert_receipt(
            account_id,
            message_id,
            from_addr,
            from_name,
            receipt,
            received_at,
        )
    }

    fn list_receipts(&self, account_id: AccountId, days: u32) -> Result<Vec<Receipt>> {
        self.list_receipts(account_id, days)
    }

    fn extract_queue(
        &self,
        account_id: AccountId,
        categories: &[&str],
        limit: usize,
    ) -> Result<Vec<ExtractQueued>> {
        self.extract_queue(account_id, categories, limit)
    }

    fn ship_extract_queue(
        &self,
        account_id: AccountId,
        limit: usize,
    ) -> Result<Vec<ExtractQueued>> {
        self.ship_extract_queue(account_id, limit)
    }

    fn ship_extract_mark(
        &self,
        account_id: AccountId,
        message_id: i64,
        marker: &str,
    ) -> Result<()> {
        self.ship_extract_mark(account_id, message_id, marker)
    }

    fn shipments_extract_apply(
        &self,
        applied: &crate::triage::extract::shipments::ShipmentsApplied,
    ) -> Result<bool> {
        self.shipments_extract_apply(applied)
    }

    fn retriage_reset(
        &self,
        account_id: AccountId,
        message_id: Option<i64>,
        days: u32,
    ) -> Result<u64> {
        self.retriage_reset(account_id, message_id, days)
    }

    fn clear_spam(&self, account_id: AccountId, message_id: i64) -> Result<bool> {
        self.clear_spam(account_id, message_id)
    }

    fn retriage_progress(&self, account_id: AccountId) -> Result<RetriageProgress> {
        self.retriage_progress(account_id)
    }

    fn extract_mark_processed(
        &self,
        account_id: AccountId,
        message_id: i64,
        extractor_model_used: &str,
    ) -> Result<()> {
        self.extract_mark_processed(account_id, message_id, extractor_model_used)
    }

    fn banking_apply(&self, applied: &BankingApplied) -> Result<i64> {
        self.banking_apply(applied)
    }

    fn marketing_apply(&self, applied: &MarketingApplied) -> Result<()> {
        self.marketing_apply(applied)
    }

    fn marketing_offers(
        &self,
        account_id: AccountId,
        days: u32,
        limit: u32,
    ) -> Result<Vec<MarketingOffer>> {
        self.marketing_offers(account_id, days, limit)
    }

    fn list_banking(&self, account_id: AccountId) -> Result<Vec<Banking>> {
        self.list_banking(account_id)
    }

    fn upsert_calendar_update(
        &self,
        account_id: AccountId,
        message_id: i64,
        calendar: &crate::triage::CalendarInfo,
        received_at: DateTime<Utc>,
    ) -> Result<i64> {
        self.upsert_calendar_update(account_id, message_id, calendar, received_at)
    }

    fn list_calendar_updates(
        &self,
        account_id: AccountId,
        hours: u32,
    ) -> Result<Vec<CalendarUpdate>> {
        self.list_calendar_updates(account_id, hours)
    }

    fn set_sender_rule(
        &self,
        account_id: AccountId,
        match_pattern: &str,
        want_text: &str,
        disposition: Disposition,
    ) -> Result<i64> {
        self.set_sender_rule(account_id, match_pattern, want_text, disposition)
    }

    fn set_sender_rule_audited(
        &self,
        account_id: AccountId,
        match_pattern: &str,
        want_text: &str,
        disposition: Disposition,
        audit: &NewAuditEntry,
    ) -> Result<i64> {
        self.set_sender_rule_audited(account_id, match_pattern, want_text, disposition, audit)
    }

    fn update_sender_rule(
        &self,
        account_id: AccountId,
        id: i64,
        match_pattern: &str,
        want_text: &str,
        disposition: Disposition,
    ) -> Result<bool> {
        self.update_sender_rule(account_id, id, match_pattern, want_text, disposition)
    }

    fn list_sender_rules(&self, account_id: AccountId) -> Result<Vec<SenderRule>> {
        self.list_sender_rules(account_id)
    }

    fn sender_rule_digest(
        &self,
        account_id: AccountId,
        from_addr: &str,
        limit: usize,
    ) -> Result<Vec<SenderDigestRow>> {
        self.sender_rule_digest(account_id, from_addr, limit)
    }

    fn ingest_message(&self, triaged: &TriagedMessage) -> Result<i64> {
        self.ingest_message(triaged)
    }

    fn ingest_message_fresh(
        &self,
        triaged: &TriagedMessage,
        scope: HealScope,
    ) -> Result<Option<i64>> {
        self.ingest_message_fresh(triaged, scope)
    }

    fn ingest_archive_message(&self, triaged: &TriagedMessage) -> Result<Option<i64>> {
        self.ingest_archive_message(triaged)
    }

    fn archive_walked_to(&self, account_id: AccountId) -> Result<Option<DateTime<Utc>>> {
        self.archive_walked_to(account_id)
    }

    fn message_is_archive(&self, account_id: AccountId, message_id: i64) -> Result<bool> {
        self.message_is_archive(account_id, message_id)
    }

    fn set_archive_walked_to(&self, account_id: AccountId, walked_to: DateTime<Utc>) -> Result<()> {
        self.set_archive_walked_to(account_id, walked_to)
    }

    fn archive_disk(&self) -> Result<crate::store::ArchiveDisk> {
        self.archive_disk()
    }

    fn archive_covered_to(&self, account_id: AccountId) -> Result<Option<DateTime<Utc>>> {
        self.archive_covered_to(account_id)
    }

    fn set_archive_covered_to(
        &self,
        account_id: AccountId,
        covered_to: DateTime<Utc>,
    ) -> Result<()> {
        self.set_archive_covered_to(account_id, covered_to)
    }

    fn stored_gmail_msg_ids(
        &self,
        account_id: AccountId,
        gmail_msg_ids: &[String],
    ) -> Result<std::collections::HashSet<String>> {
        self.stored_gmail_msg_ids(account_id, gmail_msg_ids)
    }

    fn is_known_contact(&self, account_id: AccountId, addr: &str) -> Result<bool> {
        self.is_known_contact(account_id, addr)
    }

    fn search_contacts(
        &self,
        account_id: AccountId,
        q: &str,
        limit: u32,
    ) -> Result<Vec<ContactEntry>> {
        self.search_contacts(account_id, q, limit)
    }

    fn people_named(
        &self,
        account_id: AccountId,
        name: &str,
        limit: u32,
    ) -> Result<Vec<PersonNamed>> {
        self.people_named(account_id, name, limit)
    }

    fn merge_harvested_contacts(
        &self,
        account_id: AccountId,
        batch: &[ContactEntry],
    ) -> Result<()> {
        self.merge_harvested_contacts(account_id, batch)
    }

    fn search_senders(
        &self,
        account_id: AccountId,
        q: &str,
        limit: u32,
    ) -> Result<Vec<SenderEntry>> {
        self.search_senders(account_id, q, limit)
    }

    fn search_sender_domains(
        &self,
        account_id: AccountId,
        q: &str,
        limit: u32,
    ) -> Result<Vec<SenderDomainEntry>> {
        self.search_sender_domains(account_id, q, limit)
    }

    fn sync_state(&self, account_id: AccountId, mailbox: &str) -> Result<Option<SyncState>> {
        self.sync_state(account_id, mailbox)
    }

    fn set_sync_state(
        &self,
        account_id: AccountId,
        mailbox: &str,
        state: &SyncState,
    ) -> Result<()> {
        self.set_sync_state(account_id, mailbox, state)
    }

    fn account_provider(&self, account_id: AccountId) -> Result<crate::mail::MailProvider> {
        self.account_provider(account_id)
    }

    fn sync_cursor(&self, account_id: AccountId) -> Result<Option<crate::mail::SyncCursor>> {
        self.sync_cursor(account_id)
    }

    fn set_sync_cursor(
        &self,
        account_id: AccountId,
        cursor: &crate::mail::SyncCursor,
    ) -> Result<()> {
        self.set_sync_cursor(account_id, cursor)
    }

    fn has_message(&self, account_id: AccountId, provider_msg_id: &str) -> Result<bool> {
        self.has_message(account_id, provider_msg_id)
    }

    fn inbox_unread(&self, account_id: AccountId) -> Result<Option<InboxUnread>> {
        self.inbox_unread(account_id)
    }

    fn owner_zone(&self, account_id: AccountId) -> Result<crate::owner_clock::OwnerZone> {
        self.owner_zone(account_id)
    }

    fn account_created_at(&self, account_id: AccountId) -> Result<Option<DateTime<Utc>>> {
        let conn = self.lock()?;
        let stamp: Option<String> = conn
            .query_row(
                "SELECT created_at FROM accounts WHERE id = ?1",
                params![account_id],
                |r| r.get(0),
            )
            .optional()?;
        Ok(stamp
            .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
            .map(|d| d.with_timezone(&Utc)))
    }

    fn set_inbox_unread(&self, account_id: AccountId, messages: i64, threads: i64) -> Result<()> {
        self.set_inbox_unread(account_id, messages, threads)
    }

    fn account_identity(&self, account_id: AccountId) -> Result<Option<AccountIdentity>> {
        self.account_identity(account_id)
    }

    fn account_photo(&self, account_id: AccountId) -> Result<Option<AccountPhoto>> {
        self.account_photo(account_id)
    }

    fn set_account_profile(
        &self,
        account_id: AccountId,
        name: Option<&str>,
        given_name: Option<&str>,
        photo: Option<&AccountPhoto>,
    ) -> Result<()> {
        self.set_account_profile(account_id, name, given_name, photo)
    }

    fn sealed_messages(&self, account_id: AccountId) -> Result<Vec<SealedMessage>> {
        self.sealed_messages(account_id)
    }

    fn search(
        &self,
        account_id: AccountId,
        query: &str,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<SearchHit>> {
        self.search(account_id, query, limit, offset)
    }

    #[allow(clippy::too_many_arguments)]
    fn search_filtered(
        &self,
        account_id: AccountId,
        text: &str,
        filter: &SearchFilter,
        sort: SearchSort,
        partial: bool,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<SearchHit>> {
        self.search_filtered(account_id, text, filter, sort, partial, limit, offset)
    }

    fn mark_surfaced(&self, account_id: AccountId, message_ids: &[i64]) -> Result<usize> {
        self.mark_surfaced(account_id, message_ids)
    }

    fn mark_opened(&self, account_id: AccountId, message_ids: &[i64]) -> Result<usize> {
        self.mark_opened(account_id, message_ids)
    }

    fn mark_thread_opened(&self, account_id: AccountId, thread_id: &str) -> Result<usize> {
        self.mark_thread_opened(account_id, thread_id)
    }

    fn share_open_rate(&self, account_id: AccountId, since: DateTime<Utc>) -> Result<OpenRate> {
        self.share_open_rate(account_id, since)
    }

    fn set_attention_status(
        &self,
        account_id: AccountId,
        message_id: i64,
        status: AttentionStatus,
    ) -> Result<bool> {
        self.set_attention_status(account_id, message_id, status)
    }

    fn set_reminder(
        &self,
        account_id: AccountId,
        message_id: i64,
        remind_at: DateTime<Utc>,
    ) -> Result<bool> {
        self.set_reminder(account_id, message_id, remind_at)
    }

    fn clear_reminder(&self, account_id: AccountId, message_id: i64) -> Result<bool> {
        self.clear_reminder(account_id, message_id)
    }

    fn fire_due_reminders(&self, account_id: AccountId, now: DateTime<Utc>) -> Result<Vec<i64>> {
        self.fire_due_reminders(account_id, now)
    }

    fn schedule_past_event_revisits(
        &self,
        account_id: AccountId,
        now: DateTime<Utc>,
        policy: &crate::config::RevisitPassConfig,
    ) -> Result<Vec<i64>> {
        self.schedule_past_event_revisits(account_id, now, policy)
    }

    fn resolve_sender(&self, account_id: AccountId, sender_addr: &str) -> Result<usize> {
        self.resolve_sender(account_id, sender_addr)
    }

    fn delete_sender_rule(&self, account_id: AccountId, id: i64) -> Result<bool> {
        self.delete_sender_rule(account_id, id)
    }

    fn sealed_body(&self, account_id: AccountId, message_id: i64) -> Result<SealedBody> {
        self.sealed_body(account_id, message_id)
    }

    fn set_auth_verdict(
        &self,
        account_id: AccountId,
        message_id: i64,
        verdict: Option<crate::types::AuthVerdict>,
        now: DateTime<Utc>,
    ) -> Result<()> {
        self.set_auth_verdict(account_id, message_id, verdict, now)
    }

    fn sent_listing(
        &self,
        account_id: AccountId,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<SentMessage>> {
        self.sent_listing(account_id, limit, offset)
    }

    fn messages_missing_recipients(
        &self,
        account_id: AccountId,
        limit: u32,
    ) -> Result<Vec<MessageMissingRecipients>> {
        self.messages_missing_recipients(account_id, limit)
    }

    fn blank_body_messages(
        &self,
        account_id: AccountId,
        before_id: i64,
        limit: u32,
    ) -> Result<BlankBodyScan> {
        self.blank_body_messages(account_id, before_id, limit)
    }

    fn set_message_recipients(
        &self,
        account_id: AccountId,
        message_id: i64,
        to_addrs: &str,
        cc_addrs: &str,
        bcc_addrs: &str,
    ) -> Result<bool> {
        self.set_message_recipients(account_id, message_id, to_addrs, cc_addrs, bcc_addrs)
    }

    fn append_audit(&self, account_id: AccountId, entry: &NewAuditEntry) -> Result<i64> {
        self.append_audit(account_id, entry)
    }

    fn append_event(&self, ev: &NewEvent) -> Result<Option<i64>> {
        self.append_event(ev)
    }

    fn message_has_event(&self, account_id: AccountId, message_id: i64) -> Result<bool> {
        self.message_has_event(account_id, message_id)
    }

    fn events_after(
        &self,
        account_id: AccountId,
        after_id: i64,
        limit: usize,
    ) -> Result<Vec<Event>> {
        self.events_after(account_id, after_id, limit)
    }

    fn notification_delivery_allowed(&self, account_id: AccountId, event_id: i64) -> Result<bool> {
        self.notification_delivery_allowed(account_id, event_id)
    }

    fn event_by_id(&self, account_id: AccountId, id: i64) -> Result<Option<Event>> {
        self.event_by_id(account_id, id)
    }

    fn record_notification_assessment(
        &self,
        account_id: AccountId,
        message_id: i64,
        lane: NotifyLane,
        assessment: &crate::store::NotificationAssessment,
    ) -> Result<()> {
        self.record_notification_assessment(account_id, message_id, lane, assessment)
    }

    fn latest_notification_assessment(
        &self,
        account_id: AccountId,
        message_id: i64,
        lane: NotifyLane,
    ) -> Result<Option<crate::store::NotificationAssessment>> {
        self.latest_notification_assessment(account_id, message_id, lane)
    }

    fn record_notify_decision(&self, decision: &NewNotifyDecision) -> Result<bool> {
        self.record_notify_decision(decision)
    }

    fn notify_decision_exists(
        &self,
        account_id: AccountId,
        message_id: i64,
        lane: NotifyLane,
    ) -> Result<bool> {
        self.notify_decision_exists(account_id, message_id, lane)
    }

    fn notify_decisions_since(
        &self,
        account_id: AccountId,
        since: DateTime<Utc>,
        limit: usize,
    ) -> Result<Vec<NotifyDecisionRow>> {
        self.notify_decisions_since(account_id, since, limit)
    }

    fn latest_event_id(&self, account_id: AccountId) -> Result<i64> {
        self.latest_event_id(account_id)
    }

    fn upsert_device(
        &self,
        account_id: AccountId,
        token: &str,
        platform: &str,
        tag: Option<&str>,
        environment: Option<&str>,
    ) -> Result<Device> {
        self.upsert_device(account_id, token, platform, tag, environment)
    }

    fn list_devices(&self, account_id: AccountId) -> Result<Vec<Device>> {
        self.list_devices(account_id)
    }

    fn delete_device_by_token(&self, account_id: AccountId, token: &str) -> Result<bool> {
        self.delete_device_by_token(account_id, token)
    }

    fn triage_debug(&self, account_id: AccountId, message_id: i64) -> Result<Option<TriageDebug>> {
        self.triage_debug(account_id, message_id)
    }

    fn message_unsub_fields(
        &self,
        account_id: AccountId,
        message_id: i64,
    ) -> Result<Option<MessageUnsub>> {
        self.message_unsub_fields(account_id, message_id)
    }

    fn upsert_unsubscribe(
        &self,
        account_id: AccountId,
        sender: &str,
        method: &str,
        source_message_id: Option<i64>,
        requested_at: DateTime<Utc>,
    ) -> Result<()> {
        self.upsert_unsubscribe(account_id, sender, method, source_message_id, requested_at)
    }

    fn list_unsubscribes(&self, account_id: AccountId) -> Result<Vec<UnsubscribeRecord>> {
        self.list_unsubscribes(account_id)
    }

    fn begin_unsubscribe_job(
        &self,
        account_id: AccountId,
        sender: &str,
        source_message_id: Option<i64>,
        url: &str,
        now: DateTime<Utc>,
    ) -> Result<()> {
        self.begin_unsubscribe_job(account_id, sender, source_message_id, url, now)
    }

    fn finish_unsubscribe_job(
        &self,
        account_id: AccountId,
        sender: &str,
        status: &str,
        method: &str,
        reason: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<()> {
        self.finish_unsubscribe_job(account_id, sender, status, method, reason, now)
    }

    fn mark_unsubscribe_opened(
        &self,
        account_id: AccountId,
        sender: &str,
        now: DateTime<Utc>,
    ) -> Result<bool> {
        self.mark_unsubscribe_opened(account_id, sender, now)
    }

    fn reap_unsubscribe_jobs(
        &self,
        account_id: AccountId,
        live: &[String],
        stale_before: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Result<usize> {
        self.reap_unsubscribe_jobs(account_id, live, stale_before, now)
    }

    fn set_unsubscribe_resolution(
        &self,
        account_id: AccountId,
        sender: &str,
        resolution: &str,
    ) -> Result<bool> {
        self.set_unsubscribe_resolution(account_id, sender, resolution)
    }

    fn unsubscribe_smart_block(
        &self,
        account_id: AccountId,
        sender: &str,
    ) -> Result<Option<SmartBlockMemo>> {
        self.unsubscribe_smart_block(account_id, sender)
    }

    fn set_unsubscribe_smart_block(
        &self,
        account_id: AccountId,
        sender: &str,
        proposal: Option<&SmartBlock>,
    ) -> Result<bool> {
        self.set_unsubscribe_smart_block(account_id, sender, proposal)
    }

    fn correct_triage(
        &self,
        account_id: AccountId,
        message_id: i64,
        axis: TriageAxis,
        to_value: &str,
        note: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<Option<TriageFeedback>> {
        self.correct_triage(account_id, message_id, axis, to_value, note, now)
    }

    fn list_triage_feedback(
        &self,
        account_id: AccountId,
        limit: u32,
    ) -> Result<Vec<TriageFeedback>> {
        self.list_triage_feedback(account_id, limit)
    }

    fn shred_candidates(
        &self,
        account_id: AccountId,
        cutoff: DateTime<Utc>,
        limit: u32,
    ) -> Result<Vec<ShredCandidate>> {
        self.shred_candidates(account_id, cutoff, limit)
    }

    fn shred_pending_count(&self, account_id: AccountId, cutoff: DateTime<Utc>) -> Result<i64> {
        self.shred_pending_count(account_id, cutoff)
    }

    fn record_shred(
        &self,
        account_id: AccountId,
        candidate: &ShredCandidate,
        shredded_at: DateTime<Utc>,
    ) -> Result<()> {
        self.record_shred(account_id, candidate, shredded_at)
    }

    fn shred_counts(
        &self,
        account_id: AccountId,
        recent_since: DateTime<Utc>,
    ) -> Result<(i64, i64, Option<DateTime<Utc>>)> {
        self.shred_counts(account_id, recent_since)
    }

    fn list_audit(&self, account_id: AccountId, limit: u32) -> Result<Vec<AuditEntry>> {
        self.list_audit(account_id, limit)
    }

    fn stats(&self, account_id: AccountId, bands_since: DateTime<Utc>) -> Result<StoreStats> {
        self.stats(account_id, bands_since)
    }

    fn mail_activity(
        &self,
        account_id: AccountId,
        since: DateTime<Utc>,
        until: DateTime<Utc>,
    ) -> Result<Vec<MailActivityDay>> {
        self.mail_activity(account_id, since, until)
    }

    fn stage1_apply(&self, applied: &Stage1Applied) -> Result<bool> {
        self.stage1_apply(applied)
    }

    fn notify_eligible_at(
        &self,
        account_id: AccountId,
        message_id: i64,
    ) -> Result<Option<DateTime<Utc>>> {
        self.notify_eligible_at(account_id, message_id)
    }

    fn stage1_bump_usage(
        &self,
        account_id: AccountId,
        day: &str,
        tokens: UsageTokens,
    ) -> Result<()> {
        self.stage1_bump_usage(account_id, day, tokens)
    }

    fn stage1_usage_since(&self, account_id: AccountId, since_day: &str) -> Result<Stage2Usage> {
        self.stage1_usage_since(account_id, since_day)
    }

    fn category_usage_since(
        &self,
        account_id: AccountId,
        since_day: &str,
        category: &str,
    ) -> Result<Stage2Usage> {
        self.category_usage_since(account_id, since_day, category)
    }

    #[allow(clippy::too_many_arguments)]
    fn extract_bump_usage(
        &self,
        account_id: AccountId,
        day: &str,
        category: &str,
        tokens: UsageTokens,
    ) -> Result<()> {
        self.extract_bump_usage(account_id, day, category, tokens)
    }

    fn list_usage_stage1(&self, account_id: AccountId, days: u32) -> Result<Vec<Stage2UsageDay>> {
        self.list_usage_stage1(account_id, days)
    }

    fn list_usage_by_category(
        &self,
        account_id: AccountId,
        days: u32,
    ) -> Result<Vec<(String, Vec<Stage2UsageDay>)>> {
        self.list_usage_by_category(account_id, days)
    }

    fn stage2_budget_used(&self, account_id: AccountId, thread_id: &str, day: &str) -> Result<u32> {
        self.stage2_budget_used(account_id, thread_id, day)
    }

    fn stage2_increment_budget(
        &self,
        account_id: AccountId,
        thread_id: &str,
        day: &str,
    ) -> Result<u32> {
        self.stage2_increment_budget(account_id, thread_id, day)
    }

    fn stage2_refund_budget(
        &self,
        account_id: AccountId,
        thread_id: &str,
        day: &str,
    ) -> Result<()> {
        self.stage2_refund_budget(account_id, thread_id, day)
    }

    fn stage2_bump_usage(
        &self,
        account_id: AccountId,
        day: &str,
        tokens: UsageTokens,
    ) -> Result<()> {
        self.stage2_bump_usage(account_id, day, tokens)
    }

    fn stage2_usage_today(&self, account_id: AccountId, day: &str) -> Result<Stage2Usage> {
        self.stage2_usage_today(account_id, day)
    }

    fn list_usage(&self, account_id: AccountId, days: u32) -> Result<Vec<Stage2UsageDay>> {
        self.list_usage(account_id, days)
    }

    fn stage2_usage_since(&self, account_id: AccountId, since_day: &str) -> Result<Stage2Usage> {
        self.stage2_usage_since(account_id, since_day)
    }

    fn get_app_setting(&self, account_id: AccountId, key: &str) -> Result<Option<String>> {
        self.get_app_setting(account_id, key)
    }

    fn set_app_setting(&self, account_id: AccountId, key: &str, value: &str) -> Result<()> {
        self.set_app_setting(account_id, key, value)
    }

    fn stage2_cap_overrides(&self, account_id: AccountId) -> Result<Stage2CapOverrides> {
        self.stage2_cap_overrides(account_id)
    }

    fn insert_send_tracker(
        &self,
        account_id: AccountId,
        token: &str,
        message_id: Option<i64>,
        created_at: i64,
    ) -> Result<()> {
        self.insert_send_tracker(account_id, token, message_id, created_at)
    }

    fn set_send_tracker_message(
        &self,
        account_id: AccountId,
        token: &str,
        message_id: i64,
    ) -> Result<bool> {
        self.set_send_tracker_message(account_id, token, message_id)
    }

    fn record_open(
        &self,
        account_id: AccountId,
        token: &str,
        opened_at: i64,
        user_agent: Option<&str>,
        classification: &str,
    ) -> Result<bool> {
        self.record_open(account_id, token, opened_at, user_agent, classification)
    }

    fn message_opens(&self, account_id: AccountId, message_id: i64) -> Result<Vec<MessageOpen>> {
        self.message_opens(account_id, message_id)
    }

    fn tracked_message(
        &self,
        account_id: AccountId,
        token: &str,
    ) -> Result<Option<TrackedMessage>> {
        self.tracked_message(account_id, token)
    }

    fn count_inbound_since(&self, account_id: AccountId, since: DateTime<Utc>) -> Result<u64> {
        self.count_inbound_since(account_id, since)
    }

    fn upsert_message_vector(
        &self,
        account_id: AccountId,
        message_id: i64,
        embedding: &[f32],
    ) -> Result<()> {
        self.upsert_message_vector(account_id, message_id, embedding)
    }

    fn upsert_message_vectors(&self, account_id: AccountId, items: &[(i64, &[f32])]) -> Result<()> {
        self.upsert_message_vectors(account_id, items)
    }

    fn messages_missing_vectors(
        &self,
        account_id: AccountId,
        limit: usize,
    ) -> Result<Vec<MissingVector>> {
        self.messages_missing_vectors(account_id, limit)
    }

    fn write_embedded_vectors(
        &self,
        account_id: AccountId,
        items: &[(i64, &[f32])],
    ) -> Result<usize> {
        self.write_embedded_vectors(account_id, items)
    }

    fn message_embeddable(&self, account_id: AccountId, message_id: i64) -> Result<bool> {
        self.message_embeddable(account_id, message_id)
    }

    /// Trait override so a generic `S: Store` caller resolves the CURRENT
    /// embedder, including one attached late in the background.
    fn embedder(&self) -> Option<std::sync::Arc<dyn crate::embed::Embedder>> {
        SqliteStore::embedder(self)
    }

    fn queue_translation(
        &self,
        account_id: AccountId,
        message_id: i64,
        source_lang: &str,
        deferred: bool,
        now: DateTime<Utc>,
    ) -> Result<()> {
        self.queue_translation(account_id, message_id, source_lang, deferred, now)
    }

    fn claim_translations(
        &self,
        account_id: AccountId,
        now: DateTime<Utc>,
        limit: usize,
    ) -> Result<Vec<crate::store::TranslationJob>> {
        self.claim_translations(account_id, now, limit)
    }

    fn record_translation(
        &self,
        job: &crate::store::TranslationJob,
        write: &crate::store::TranslationWrite<'_>,
        now: DateTime<Utc>,
    ) -> Result<bool> {
        self.record_translation(job, write, now)
    }

    fn read_languages(&self, account_id: AccountId) -> Result<Vec<String>> {
        self.read_languages(account_id)
    }

    fn set_read_languages(
        &self,
        account_id: AccountId,
        codes: &[String],
        now: DateTime<Utc>,
    ) -> Result<Vec<String>> {
        self.set_read_languages(account_id, codes, now)
    }

    fn promote_deferred_translations(
        &self,
        account_id: AccountId,
        thread_id: &str,
        now: DateTime<Utc>,
    ) -> Result<usize> {
        self.promote_deferred_translations(account_id, thread_id, now)
    }

    fn message_translations(
        &self,
        account_id: AccountId,
        message_ids: &[i64],
    ) -> Result<Vec<crate::store::MessageTranslation>> {
        self.message_translations(account_id, message_ids)
    }

    fn upsert_translation_vector(
        &self,
        account_id: AccountId,
        message_id: i64,
        embedding: &[f32],
    ) -> Result<()> {
        self.upsert_translation_vector(account_id, message_id, embedding)
    }

    fn translations_missing_vectors(
        &self,
        account_id: AccountId,
        limit: usize,
    ) -> Result<Vec<crate::store::MissingVector>> {
        self.translations_missing_vectors(account_id, limit)
    }
}
