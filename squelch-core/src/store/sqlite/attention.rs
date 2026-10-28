//! The attention ledger: ranked/attention updates, sitrep bands, surfacing
//! stamps and store stats.

use super::*;

// Legacy buckets remain wire-compatible, but new mail is projected from the
// current agent decision. A neutral ingest row is pending, never classified noise.
//
// The account's own mail in the inbound listing (see `OWN_MAIL`) is the one row
// that is neither: it gets an access review and never a decision, so without
// its own arm it would count as pending for good. It reads as noise, the tier
// ingest writes on the sent copies it resembles.
//
// The archive is not in it at all. Those rows have no decision and never will
// unprompted, so counted here they would be a year of mail reading as pending
// on every stats card and activity chart this feeds.
pub(super) const CANONICAL_MAIL_CTE: &str = concat!(
    "WITH ",
    auth_fye_threads_cte!(),
    ", classified AS (
    SELECT m.*, CASE
      WHEN ",
    own_mail_sql!(),
    " THEN 'noise'
      WHEN a.message_id IS NULL THEN COALESCE(t.tier,'pending')
      WHEN d.message_id IS NULL THEN 'pending'
      WHEN COALESCE(pref.show_in_fye,CASE WHEN q.id IS NOT NULL THEN 1 WHEN ",
    auth_mail_sql!(),
    " THEN 0
        ELSE json_extract(att.attention_json,'$.show_in_fye') END,0)=1 THEN 'signal'
      ELSE 'noise' END AS display_tier
    FROM messages m
    LEFT JOIN triage t ON t.account_id=m.account_id AND t.message_id=m.id
    LEFT JOIN agent_message_state a ON a.account_id=m.account_id AND a.message_id=m.id
    LEFT JOIN agent_message_decisions d ON d.account_id=a.account_id AND d.message_id=a.message_id AND d.revision=a.revision
    LEFT JOIN agent_thread_attention att ON att.account_id=m.account_id AND att.thread_id=m.thread_id
    LEFT JOIN agent_thread_preferences pref ON pref.account_id=m.account_id AND pref.thread_id=m.thread_id
    LEFT JOIN auth_fye_threads q ON q.account_id=m.account_id AND q.thread_key=",
    thread_key_sql!("m"),
    "
    WHERE m.is_archive=0
)"
);

/// THE OWNER'S DONE, as one statement: `status='done'`, `resolved_at = ?1`,
/// `resolved_by` cleared and the fired reminder retired, for message `?3` of
/// account `?2` AND every sibling in its thread (`''` is no thread) except a
/// sibling that holds FYE on its own (`holds_fye_triage_sql!`). Why each part, in
/// [`SqliteStore::set_attention_status`]'s Done arm. It lives in ONE place
/// because three writers must close mail identically: that arm, the
/// deferral in `set_reminder`, and the triage agent's auto-done
/// (`agent_triage::apply_done`), which is the owner's Done made for them and
/// must leave the bands exactly as the owner's own would.
macro_rules! done_thread_sql {
    () => {
        concat!(
            "UPDATE triage
     SET status = 'done', resolved_at = ?1, resolved_by = NULL, reminded_at = NULL
     WHERE account_id = ?2
       AND (message_id = ?3 OR (message_id IN (
           SELECT sib.id FROM messages me
           JOIN messages sib ON sib.account_id = me.account_id
                            AND sib.thread_id = me.thread_id
           WHERE me.account_id = ?2 AND me.id = ?3
             AND me.thread_id != '') AND NOT ",
            holds_fye_triage_sql!(),
            "))"
        )
    };
}
pub(super) const DONE_THREAD_SQL: &str = done_thread_sql!();

/// [`DONE_THREAD_SQL`] for the triage agent's auto-done: the same write, minus
/// rows already `done`. The owner re-doing a closed row takes it over (its
/// `resolved_by` cleared, its `resolved_at` now); the agent is not the owner,
/// so a smart-cleared sibling keeps its batch (and that batch's undo), and a
/// row the owner closed keeps the time they closed it.
pub(super) const AGENT_DONE_THREAD_SQL: &str = concat!(done_thread_sql!(), " AND status != 'done'");

/// The recency window every human-door listing runs under (`?2` is the caller's
/// `since`), plus the ONE exemption from it: a row carrying a reminder, pending
/// or fired.
///
/// THE WINDOW EXISTS SO OLD MAIL CANNOT RE-LITIGATE ITSELF — a sitrep that
/// reaches back over the whole corpus is not a sitrep. A reminder is the exact
/// opposite case: the user pointed at that mail themselves and said "show me
/// this again", so it has to outlive the window or the feature's core promise
/// dies on day 30. Without the exemption the pending lens stops listing parked
/// mail a month after it was RECEIVED (not a month after it was parked), and a
/// reminder set on older mail — the "next month" pick on a two-month-old
/// thread — fires into a row that no band, no count and no page will ever show.
///
/// Like [`STANDING_BAND`] it lives in ONE place because the list query and the
/// header's count must agree row for row. [`WINDOW_ROWS`] is the same three
/// arms as a set to drive a count from: AN ARM ADDED HERE NEEDS ITS BRANCH
/// THERE, or the rows only it admits never reach it.
pub(super) const WITHIN_WINDOW: &str = "(m.received_at >= ?2
            OR t.remind_at IS NOT NULL
            OR t.reminded_at IS NOT NULL)";

/// The message ids [`WITHIN_WINDOW`] can admit for account `?1`, ONE BRANCH
/// PER ARM, each from its own index: mail received since `?2`
/// (`idx_messages_received`, covering), and rows with a reminder set or fired
/// (the two partial reminder indexes, which `migrate` creates on every open
/// with the columns they cover). A superset is all a driver needs, because
/// the predicate itself is still applied, so `UNION ALL`: an id two arms
/// admit twice counts once under the counts' own DISTINCT.
/// `band_counts_match_the_unwindowed_scan` covers every arm.
pub(super) const WINDOW_ROWS: &str = "SELECT id FROM messages INDEXED BY idx_messages_received
              WHERE account_id = ?1 AND received_at >= ?2
             UNION ALL
             SELECT message_id FROM triage INDEXED BY idx_triage_remind_at
              WHERE account_id = ?1 AND remind_at IS NOT NULL
             UNION ALL
             SELECT message_id FROM triage INDEXED BY idx_triage_reminded_at
              WHERE account_id = ?1 AND reminded_at IS NOT NULL";

/// The archive tier's exclusion from the human inventory, with the same
/// exemption [`WITHIN_WINDOW`] gives: a reminder the user set on an archive
/// message they found by search is their own instruction to see it again, so
/// that row lists (and counts) exactly as a live row with a reminder does.
pub(super) const LIVE_OR_REMINDER: &str = "(m.is_archive = 0
            OR t.remind_at IS NOT NULL
            OR t.reminded_at IS NOT NULL)";

/// Membership test for the `standing` band, written against the `triage t` /
/// `messages m` join that both band sites share. It lives in ONE place because
/// the list query and the header's count must agree row for row, or the sitrep
/// header contradicts the list it heads.
///
/// STANDING IS A PROPERTY, NOT A TIMESTAMP: the band is mail owed the user's
/// attention — a dated obligation (`past_due`/`deadline`), a fired reminder, or
/// live correspondence: a thread the user has written in, or non-marketing mail
/// from a sender the user has written to. A dateless "can you send me the form?"
/// from a real correspondent is exactly as owed as a bill, and the surfacing
/// clock must never rotate it out. Because this is a definition over stored
/// rows, both widening it and narrowing it are RETROACTIVE: mail already triaged
/// joins or leaves the band on the next read, with no re-triage and no backfill.
///
/// THE REMINDER ARM (`reminded_at IS NOT NULL`) carries no tier test on purpose.
/// Every other arm infers what the user cares about; this one is the user saying
/// it outright, so a noise-tier newsletter they asked to see again comes back
/// exactly as loudly as a past-due bill. It is the fired stamp and not the
/// pending one because a pending reminder means "not now" — `set_reminder`
/// marks the thread done precisely to get it out of the bands until it is due.
///
/// Human inventory includes restricted mail. The user's own sent messages remain
/// excluded: a sent sibling supplies correspondence context, while `m.is_sent = 0`
/// keeps it out of the incoming listing.
///
/// THE HAVING-WRITTEN-TO-THEM ARM IS NOT A MARKETING PASS. It is the weakest
/// arm by construction — one email, ever, to an address — and it carries no
/// tier or importance floor, so on its own it hands a permanent standing-band
/// seat to every shop whose `info@` the user once asked about an order. That is
/// the arm working as designed for a person and backwards for a mailing list:
/// the guarantee exists to rescue BELOW-THRESHOLD mail from someone the user
/// actually corresponds with, and a promotional blast is not that mail however
/// it arrived. So the arm alone is gated on `category != 'marketing'`.
///
/// The gate is the CLASSIFIER'S verdict and deliberately not the
/// `List-Unsubscribe` header, which reads like a newsletter marker and is not
/// one: on a real mailbox it is carried by rental check-ins, eBay delivery
/// updates, support-ticket receipts and a friend's WeTransfer link. Suppressing
/// on the header would take out exactly the below-threshold transactional mail
/// this arm exists to keep. A newsletter the classifier labels `general` stays
/// in the band, and the fix for that is the classifier, not a second heuristic
/// here.
///
/// `COALESCE(..., '')` because the comparison is three-valued: `category` is
/// NULL on every row triage never reached and on rows older than the column,
/// and a bare `!=` against NULL is NULL, which is not TRUE, which would drop
/// those rows out of the band — the opposite of the intent.
///
/// The other three arms are ungated on purpose. A dated obligation, a reminder
/// the user set themselves, and a thread the user has WRITTEN IN are each a
/// stronger claim than the category is: mail the user replied to is a
/// conversation they joined, whatever it was classified as on arrival.
///
/// Address matching folds case on BOTH sides: `messages.from_addr` is stored as
/// the header spelled it, and `contacts.addr` is lowercased by the Sent-history
/// harvest but kept verbatim by the per-message Sent ingest, so neither side can
/// be assumed normalized. That `COLLATE NOCASE` is served by
/// `idx_contacts_addr_nocase` and by nothing else: the primary key is BINARY,
/// and without the collated index this correlated probe walks every contact
/// once per message in the window. `pub(super)` so the plan test can pin that.
pub(super) const STANDING_BAND: &str = "(t.tier IN ('past_due','deadline')
        OR t.reminded_at IS NOT NULL
        OR (m.thread_id != '' AND EXISTS(
                SELECT 1 FROM messages s
                WHERE s.account_id = m.account_id
                  AND s.thread_id = m.thread_id
                  AND s.is_sent = 1))
        OR (COALESCE(t.category, '') != 'marketing' AND EXISTS(
                SELECT 1 FROM contacts c
                WHERE c.account_id = t.account_id
                  AND c.addr = lower(trim(m.from_addr)) COLLATE NOCASE
                  AND c.sent_count > 0)))
       AND t.status != 'done'";

/// Sitrep band counts over non-sealed rows, one per THREAD, hence DISTINCT
/// thread keys (blank thread_id falls back to the message id), over the
/// received_at window, hence `?2` AND the [`WITHIN_WINDOW`] reminder
/// exemption: a fired reminder still counts however old the mail is.
/// Standing is decided in the subquery, where the `m` join alias the shared
/// expression is written against is still in scope.
///
/// DRIVEN FROM THE WINDOW: `w` is [`WINDOW_ROWS`], the rows
/// [`WITHIN_WINDOW`] can admit, so the count reads a month of mail rather
/// than every message in the store. Each is then one rowid probe of `triage`
/// and `messages`, and the predicates the list shares with it apply
/// unchanged, [`WITHIN_WINDOW`] included, so the narrowing cannot change a
/// count (`band_counts_match_the_unwindowed_scan`).
pub(super) static BANDS_SQL: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    format!(
        "SELECT
             COUNT(DISTINCT CASE WHEN t.standing = 1 THEN tkey END),
             COUNT(DISTINCT CASE WHEN t.surfaced_at IS NULL
                 AND t.status != 'done' THEN tkey END),
             COUNT(DISTINCT CASE WHEN t.status = 'open' THEN tkey END)
         FROM (SELECT t.*, COALESCE(NULLIF(m.thread_id, ''), 'msg-' || m.id) AS tkey,
                      ({STANDING_BAND}) AS standing
               FROM ({WINDOW_ROWS}) w
               CROSS JOIN triage t ON t.message_id = w.id
               CROSS JOIN messages m ON m.id = t.message_id
               WHERE t.account_id = ?1
                 AND m.is_sent = 0
                 AND m.is_spam = 0
                 AND {LIVE_OR_REMINDER}
                 AND {WITHIN_WINDOW}) t"
    )
});

/// The all-time half of [`StoreStats`]: everything but the windowed bands.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct InventoryCounts {
    pub(super) tier_counts: std::collections::BTreeMap<String, i64>,
    pub(super) sealed: i64,
    pub(super) spam: i64,
    pub(super) spam_synced_at: Option<DateTime<Utc>>,
    pub(super) last_history_id: Option<i64>,
    pub(super) last_surfaced_at: Option<DateTime<Utc>>,
}

/// [`InventoryCounts`] as last read, for one account, at one [`WriteToken`].
#[derive(Debug)]
pub(super) struct InventoryMemo {
    pub(super) account: AccountId,
    pub(super) token: WriteToken,
    pub(super) counts: InventoryCounts,
}

/// Read [`InventoryCounts`] from the store.
fn inventory_counts(conn: &Connection, account_id: AccountId) -> Result<InventoryCounts> {
    let mut tier_counts = std::collections::BTreeMap::new();
    {
        // JOINED TO `messages` for the two exclusions, which this count did
        // without for a long time and should not have. Sent mail and
        // provider spam are both written tier=noise by ingest without a
        // model ever looking at them, and neither is listed by the noise
        // page, so counting them made the header's noise number a promise
        // the page could not keep.
        let mut stmt = conn.prepare(&format!(
            "{CANONICAL_MAIL_CTE} SELECT display_tier, COUNT(*) FROM classified
             WHERE account_id=?1 AND is_sent=0 AND is_spam=0 GROUP BY display_tier"
        ))?;
        let rows = stmt.query_map(params![account_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })?;
        for row in rows {
            let (tier, n) = row?;
            tier_counts.insert(tier, n);
        }
    }

    let sealed: i64 = conn.query_row(
        "SELECT COUNT(*) FROM agent_message_state a
         JOIN agent_message_decisions d ON d.account_id=a.account_id AND d.message_id=a.message_id AND d.revision=a.revision
         WHERE a.account_id=?1 AND a.access='restricted'
           AND EXISTS(SELECT 1 FROM json_each(d.decision_json,'$.auth.kinds')
             WHERE value IN ('otp','password_reset','sign_in_link','verification'))",
        params![account_id],
        |r| r.get(0),
    )?;

    // WHEN THE SPAM FOLDER WAS LAST FETCHED, which the page needs in order to
    // tell "we looked and it is empty" from "nobody has looked yet".
    let spam_synced_at: Option<String> = conn
        .query_row(
            "SELECT value FROM app_settings WHERE account_id = ?1 AND key = ?2",
            params![account_id, SPAM_SYNCED_AT_KEY],
            |r| r.get(0),
        )
        .optional()?;
    let spam_synced_at = spam_synced_at.as_deref().and_then(|s| parse_dt(s).ok());

    // THE SPAM PAGE'S DOOR NUMBER. Counted the same way the human page lists,
    // served by `idx_messages_spam` — so the chip and the
    // page it opens cannot disagree.
    let spam: i64 = conn.query_row(
        "SELECT COUNT(*) FROM messages m
         JOIN triage t ON t.message_id = m.id
         WHERE m.account_id=?1 AND m.is_spam = 1",
        params![account_id],
        |r| r.get(0),
    )?;

    let last_history_id: Option<i64> = conn
        .query_row(
            "SELECT last_uid FROM sync_state WHERE account_id=?1 AND mailbox='history'",
            params![account_id],
            |r| r.get(0),
        )
        .optional()?;

    let last_surfaced_at = conn.query_row(
        "SELECT MAX(surfaced_at) FROM triage
         WHERE account_id = ?1",
        params![account_id],
        |r| dt_opt(r, 0),
    )?;

    Ok(InventoryCounts {
        tier_counts,
        sealed,
        spam,
        spam_synced_at,
        last_history_id,
        last_surfaced_at,
    })
}

impl SqliteStore {
    pub(super) fn mark_surfaced(
        &self,
        account_id: AccountId,
        message_ids: &[i64],
    ) -> Result<usize> {
        if message_ids.is_empty() {
            return Ok(0);
        }
        let mut conn = self.lock()?;
        let now = Utc::now().to_rfc3339();
        let tx = conn.transaction()?;
        let mut first_surfaced = 0usize;
        {
            // Stamp surfaced_at once and promote new to open. This is a human
            // acknowledgement, independent of external agent access.
            let mut stmt = tx.prepare(
                "UPDATE triage
                 SET surfaced_at = COALESCE(surfaced_at, ?1),
                     status = CASE WHEN status = 'new' THEN 'open' ELSE status END
                 WHERE account_id = ?2 AND message_id = ?3

                   AND surfaced_at IS NULL",
            )?;
            for &id in message_ids {
                first_surfaced += stmt.execute(params![now, account_id, id])?;
            }
        }
        tx.commit()?;
        Ok(first_surfaced)
    }

    /// Stamp `opened_at` on rows the user has opened.
    ///
    /// Deliberately NOT folded into [`Self::mark_surfaced`], which every list
    /// door calls: the two answer different questions, and merging them would
    /// make "opened" mean "appeared in a list" (see the column comment in
    /// `schema.sql`).
    ///
    /// FIRST OPEN ONLY, like `surfaced_at`: re-reading a thread next week says
    /// nothing new about whether it needed opening, and a moving stamp would
    /// make the rate drift with re-reads. Sealed rows are excluded, which is
    /// belt and braces - the human door never serves one through this path -
    /// and it keeps the denominator and numerator counting the same universe.
    pub(super) fn mark_opened(&self, account_id: AccountId, message_ids: &[i64]) -> Result<usize> {
        if message_ids.is_empty() {
            return Ok(0);
        }
        let mut conn = self.lock()?;
        let now = Utc::now().to_rfc3339();
        let tx = conn.transaction()?;
        let mut first_opened = 0usize;
        {
            let mut stmt = tx.prepare(
                "UPDATE triage
                 SET opened_at = ?1
                 WHERE account_id = ?2 AND message_id = ?3

                   AND opened_at IS NULL",
            )?;
            for &id in message_ids {
                first_opened += stmt.execute(params![now, account_id, id])?;
            }
        }
        tx.commit()?;
        Ok(first_opened)
    }

    /// Stamp every message in one thread opened, by thread id.
    ///
    /// THE THREAD, NOT A LIST OF IDS, because the client should not have to
    /// tell the daemon which messages a thread holds - it would be sending back
    /// a list the daemon gave it, and the two could disagree after a sync. The
    /// reader shows the whole thread oldest-first anyway, so opening it is
    /// opening all of it.
    ///
    /// Returns the first-open count.
    pub(super) fn mark_thread_opened(
        &self,
        account_id: AccountId,
        thread_id: &str,
    ) -> Result<usize> {
        let conn = self.lock()?;
        // The sealed guard and the once-only guard are the same ones
        // `mark_opened` keeps, in one statement because the ids are a subquery
        // rather than a caller's list.
        let n = conn.execute(
            "UPDATE triage
                SET opened_at = ?1
              WHERE account_id = ?2

                AND opened_at IS NULL
                AND message_id IN (
                    SELECT id FROM messages WHERE account_id = ?2 AND thread_id = ?3
                )",
            params![Utc::now().to_rfc3339(), account_id, thread_id],
        )?;
        Ok(n)
    }

    /// How much of this mailbox's incoming mail the user has had to open, over
    /// the rows received since `since`.
    ///
    /// WHAT IT COUNTS, and every exclusion is a decision:
    ///
    /// - RECEIVED MAIL ONLY (`is_sent = 0`). Nobody opens their own outbox, and
    ///   counting it would dilute the rate with a number that has no opinion.
    /// - AND NOT PROVIDER SPAM (`is_spam = 0`), which would dilute it far
    ///   harder and in the flattering direction: spam is unopened by definition
    ///   and there is a lot of it, so counting it would drive "mail the user had
    ///   to open themselves" toward zero on volume alone.
    /// - SEALED MAIL IS IN THE DENOMINATOR. A login code is mail that arrived
    ///   and did not need opening, which is the most honest example there is of
    ///   the thing being measured. It can never be in the numerator, because
    ///   [`Self::mark_opened`] refuses to stamp it.
    /// - Rows with no triage row at all are absent from both, by the join: a
    ///   message the daemon never triaged is one it cannot speak for.
    ///
    /// AND IT NEVER LOOKS BACK FURTHER THAN THE LEDGER ITSELF. `opened_at` was
    /// added to an existing product, so on the day it ships every mailbox has
    /// years of mail and no opens at all; a window that reached past the
    /// column's own arrival would divide a full denominator by an empty
    /// numerator and report that this person opens almost nothing. The caller's
    /// sample floors cannot catch that - they measure how old the MAIL is,
    /// which is ample, not how old the LEDGER is, which is seconds. See
    /// `migrate::stamp_open_ledger_start`.
    ///
    /// WHAT IT CANNOT SEE is mail read somewhere else. `opened_at` is stamped
    /// by this user's own Passband client and nowhere else, so somebody who
    /// reads half their mail in Gmail on a phone will find this FLATTERINGLY
    /// LOW.
    /// Every consumer has to carry that caveat with it, and the one consumer
    /// (the invite mail) does.
    pub(super) fn share_open_rate(
        &self,
        account_id: AccountId,
        since: DateTime<Utc>,
    ) -> Result<OpenRate> {
        let conn = self.lock()?;
        // The window, clamped to the ledger. Whichever floor is LATER wins: a
        // caller asking for ninety days of a thirty-day-old ledger gets thirty.
        let ledger_since = open_ledger_since(&conn, account_id)?;
        let floor = since.max(ledger_since);
        let floor_text = floor.to_rfc3339();
        let (received, opened): (i64, i64) = conn.query_row(
            "SELECT COUNT(*), COUNT(t.opened_at)
               FROM messages m JOIN triage t ON t.message_id = m.id
              WHERE m.account_id = ?1 AND m.is_sent = 0 AND m.is_spam = 0
                AND m.is_archive = 0
                AND m.received_at >= ?2",
            params![account_id, floor_text],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        // The oldest received row in the window, which is what says how much
        // history the answer actually rests on. A mailbox synced three days ago
        // has a rate; it does not have a rate worth mailing to a stranger.
        let oldest: Option<String> = conn.query_row(
            "SELECT MIN(received_at) FROM messages
              WHERE account_id = ?1 AND is_sent = 0 AND is_spam = 0
                AND is_archive = 0
                AND received_at >= ?2",
            params![account_id, floor_text],
            |r| r.get(0),
        )?;
        Ok(OpenRate {
            received: received.max(0) as u64,
            opened: opened.max(0) as u64,
            oldest_received_at: oldest.and_then(|s| {
                DateTime::parse_from_rfc3339(&s)
                    .ok()
                    .map(|d| d.with_timezone(&Utc))
            }),
        })
    }

    pub(super) fn set_attention_status(
        &self,
        account_id: AccountId,
        message_id: i64,
        status: AttentionStatus,
    ) -> Result<bool> {
        let conn = self.lock()?;
        // Done stamps resolved_at; reopening clears it. Human actions also apply
        // to mail restricted from external agents.
        //
        // EITHER WAY `resolved_by` IS CLEARED, on every row the write touches:
        // a person re-doing or reopening a row an automatic sweep resolved has
        // taken it over, and that sweep's undo must no longer reach it.
        let resolved_at = match status {
            AttentionStatus::Done => Some(Utc::now().to_rfc3339()),
            _ => None,
        };
        // DONE RESOLVES THE WHOLE THREAD: the bands show one row per thread,
        // so resolving the representative must also resolve
        // its siblings — otherwise a hidden sibling pops straight back into the
        // band and done becomes whack-a-mole. Reopen (open/new) stays
        // message-scoped: the undo path restores exactly the one row it removed,
        // which is enough to re-surface the thread.
        //
        // DONE ALSO RETIRES THE FIRED REMINDER. `reminded_at` is what holds a
        // row in the standing band at any tier, so a stamp left behind means a
        // reminder the user answered comes back every time the row is ever
        // reopened — the reminder fired, it was dealt with, it is spent. Reopen
        // does not restore it for the same reason: undo restores the row, not
        // the schedule.
        let n = match status {
            // A SIBLING that holds FYE on its own (`holds_fye_triage_sql!`: an
            // unruled critical account change, or mail ruled "Not me") is the
            // one exception to the sweep: the Auth page archives the sign-in
            // alert it used, and Gmail threads that alert with the
            // recovery-phone change beside it, which must hold until the owner
            // rules on it. Done on that message itself is the owner's own act
            // and applies.
            AttentionStatus::Done => conn.execute(
                DONE_THREAD_SQL,
                params![resolved_at, account_id, message_id],
            )?,
            _ => conn.execute(
                "UPDATE triage
                 SET status = ?1, resolved_at = ?2, resolved_by = NULL
                 WHERE account_id = ?3 AND message_id = ?4",
                params![status.as_str(), resolved_at, account_id, message_id],
            )?,
        };
        Ok(n > 0)
    }

    /// "Remind me about this at T." Thread-wide done PLUS the stamp, in one
    /// transaction: a reminder that half-applied would either nag a mail the
    /// user just deferred or lose the deferral entirely, and both are worse than
    /// the write failing.
    ///
    /// THE STAMP GOES FIRST, and its row count — not the done sweep's — is the
    /// missing/sealed answer. The sweep is thread-wide, so a sealed target with
    /// an unsealed sibling would report `true` off a row the caller never named
    /// while the stamp itself landed nowhere.
    pub(super) fn set_reminder(
        &self,
        account_id: AccountId,
        message_id: i64,
        remind_at: DateTime<Utc>,
    ) -> Result<bool> {
        let mut conn = self.lock()?;
        let now = Utc::now().to_rfc3339();
        let tx = conn.transaction()?;
        // Human reminders apply to restricted mail too. Sent mail and provider
        // spam have separate inventories, so they remain outside this schedule.
        // A new reminder replaces the previous pending or fired occurrence.
        let stamped = tx.execute(
            "UPDATE triage
             SET remind_at = ?1, reminded_at = NULL
             WHERE account_id = ?2 AND message_id = ?3
               AND EXISTS(SELECT 1 FROM messages mm
                          WHERE mm.account_id = ?2 AND mm.id = ?3
                            AND mm.is_sent = 0 AND mm.is_spam = 0)",
            params![remind_at.to_rfc3339(), account_id, message_id],
        )?;
        if stamped == 0 {
            return Ok(false);
        }
        // DEFERRING IS RESOLVING, thread-wide, with the SAME SQL as
        // `set_attention_status`'s Done arm and for the same reason: the bands
        // show one row per thread, so leaving a sibling open puts the mail the
        // user just snoozed straight back in front of them. It clears
        // `reminded_at` for that same reason — a sibling still wearing an old
        // fired stamp sits in the standing band while the thread is supposedly
        // parked until the new date.
        // A sibling that holds FYE is spared, as in the Done arm.
        tx.execute(DONE_THREAD_SQL, params![now, account_id, message_id])?;
        tx.commit()?;
        Ok(true)
    }

    /// Drop a PENDING reminder, leaving everything else exactly where it is.
    ///
    /// Deliberately NOT the inverse of [`Self::set_reminder`]: it does not reopen
    /// the thread (the user may well have meant to resolve it) and it does not
    /// touch `reminded_at` (clearing a pending reminder says nothing about one
    /// that already came due). Idempotent — a row with no reminder is a
    /// successful no-op, so only a missing row returns `false`.
    pub(super) fn clear_reminder(&self, account_id: AccountId, message_id: i64) -> Result<bool> {
        let conn = self.lock()?;
        let n = conn.execute(
            "UPDATE triage
             SET remind_at = NULL
             WHERE account_id = ?1 AND message_id = ?2",
            params![account_id, message_id],
        )?;
        Ok(n > 0)
    }

    /// THE SWEEP: every reminder now due, fired in one statement, returning the
    /// message ids it moved.
    ///
    /// The stamp MOVES rather than being copied — `remind_at` NULLs as
    /// `reminded_at` takes its value — which is what makes this idempotent
    /// without a cooldown: a fired row no longer matches the predicate, so a
    /// second tick (or a second daemon) cannot fire it twice. `resolved_at`
    /// clears alongside the status because a reopened row that still carries a
    /// resolution timestamp reads as done to everything downstream.
    ///
    /// `now` is a parameter, not `Utc::now()`, so a test can hold the clock.
    pub(super) fn fire_due_reminders(
        &self,
        account_id: AccountId,
        now: DateTime<Utc>,
    ) -> Result<Vec<i64>> {
        let conn = self.lock()?;
        // An explicit reminder is sufficient authority to resurface human mail.
        let mut stmt = conn.prepare(
            "UPDATE triage
             SET status = 'open',
                 resolved_at = NULL,
                 resolved_by = NULL,
                 reminded_at = remind_at,
                 remind_at = NULL
             WHERE account_id = ?1
               AND remind_at IS NOT NULL
               AND remind_at <= ?2

             RETURNING message_id",
        )?;
        let out = stmt
            .query_map(params![account_id, now.to_rfc3339()], |r| {
                r.get::<_, i64>(0)
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(out)
    }

    pub(super) fn resolve_sender(&self, account_id: AccountId, sender_addr: &str) -> Result<usize> {
        let sender = sender_addr.trim().to_lowercase();
        if sender.is_empty() {
            return Ok(0);
        }
        let conn = self.lock()?;
        // Already-done rows are left alone so their original resolved_at — and
        // whatever resolved them — survives. This is an explicit human sweep,
        // but of a SENDER, not a ruling: auth mail from it that holds FYE
        // (`holds_fye_triage_sql!`) stays.
        let n = conn.execute(
            concat!(
                "UPDATE triage
             SET status = 'done', resolved_at = ?1, resolved_by = NULL
             WHERE account_id = ?2

               AND status != 'done'
               AND message_id IN (
                   SELECT m.id FROM messages m
                   WHERE m.account_id = ?2
                     AND LOWER(TRIM(m.from_addr)) = ?3
               ) AND NOT ",
                holds_fye_triage_sql!()
            ),
            params![Utc::now().to_rfc3339(), account_id, sender],
        )?;
        // AND THE OWNER TAKES OVER what an automatic sweep had already
        // resolved for this sender. Those rows are done either way; what
        // changes is whose done it is. Left tagged, a later undo of that
        // sweep would reopen mail from a sender the owner has just
        // unsubscribed from or muted. Not counted in the return, which is
        // the rows this call newly resolved.
        conn.execute(
            "UPDATE triage
             SET resolved_by = NULL
             WHERE account_id = ?1
               AND status = 'done' AND resolved_by IS NOT NULL
               AND message_id IN (
                   SELECT m.id FROM messages m
                   WHERE m.account_id = ?1
                     AND LOWER(TRIM(m.from_addr)) = ?2
               )",
            params![account_id, sender],
        )?;
        Ok(n)
    }

    /// `bands_since` windows ONLY the band counts, mirroring the `since` the
    /// list queries run under: standing's correspondence arms admit mail from
    /// every contact the user has ever written to, so an unwindowed count
    /// grows monotonically with corpus age and outruns the list it heads. The
    /// inventory counts (tiers, total, sealed) stay all-time on purpose —
    /// they describe the store, not the sitrep.
    ///
    /// THE INVENTORY IS REUSED UNTIL SOMETHING IS WRITTEN. It is the expensive
    /// half (the tier count reads every message in the store) and depends on
    /// nothing but stored rows, so it is kept with the [`WriteToken`] it was
    /// read at and served again while the token is unchanged: the client's
    /// poll and the metrics scrape on a mailbox with nothing new. The bands
    /// are recomputed every call, because their window moves with the clock.
    pub(super) fn stats(
        &self,
        account_id: AccountId,
        bands_since: DateTime<Utc>,
    ) -> Result<StoreStats> {
        let conn = self.lock()?;
        let token = WriteToken::of(&conn)?;
        let inventory = {
            // A cache: a poisoned lock only means a panic mid-store, and the
            // token decides what in it is still good.
            let mut memo = self
                .inventory_memo
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            match memo.as_ref() {
                Some(kept) if kept.account == account_id && kept.token == token => {
                    kept.counts.clone()
                }
                _ => {
                    let counts = inventory_counts(&conn, account_id)?;
                    *memo = Some(InventoryMemo {
                        account: account_id,
                        token,
                        counts: counts.clone(),
                    });
                    counts
                }
            }
        };
        let InventoryCounts {
            tier_counts,
            sealed,
            spam,
            spam_synced_at,
            last_history_id,
            last_surfaced_at,
        } = inventory;
        let total: i64 = tier_counts.values().sum();

        let (standing, new_count, open_count): (i64, i64, i64) =
            conn.prepare_cached(&BANDS_SQL)?
                .query_row(params![account_id, bands_since.to_rfc3339()], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                })?;

        Ok(StoreStats {
            tier_counts,
            total,
            sealed,
            spam,
            spam_synced_at,
            last_history_id: last_history_id.map(|v| v as u64),
            bands: BandCounts {
                standing,
                new: new_count,
                open: open_count,
            },
            last_surfaced_at,
        })
    }

    /// See [`Store::mail_activity`]. One pass over the window's messages,
    /// bucketed in SQL: `received_at` is stored as RFC3339 UTC, so its first
    /// ten characters ARE the ledger's day key, and the bounds compare as text
    /// against the same `to_rfc3339` shape the writes use.
    ///
    /// LEFT JOIN, not JOIN: a message the triage pipeline has not reached yet
    /// is still mail that arrived, so it counts as received and in no tier.
    /// Sent mail carries a neutral triage row (see `ingest`: "the user's own
    /// outbox must never pollute the ranked inbox"), which is why every tier
    /// bucket re-checks `is_sent = 0` rather than trusting the row's tier.
    pub(super) fn mail_activity(
        &self,
        account_id: AccountId,
        since: DateTime<Utc>,
        until: DateTime<Utc>,
    ) -> Result<Vec<MailActivityDay>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(&format!(
            "{CANONICAL_MAIL_CTE} SELECT substr(m.received_at, 1, 10) AS day,
                    COALESCE(SUM(m.is_sent = 0), 0),
                    COALESCE(SUM(m.is_sent = 1), 0),
                    COALESCE(SUM(m.is_sent = 0 AND EXISTS(SELECT 1 FROM agent_message_state a
                      JOIN agent_message_decisions d ON d.account_id=a.account_id AND d.message_id=a.message_id AND d.revision=a.revision
                      WHERE a.account_id=m.account_id AND a.message_id=m.id AND a.access='restricted'
                        AND EXISTS(SELECT 1 FROM json_each(d.decision_json,'$.auth.kinds')
                          WHERE value IN ('otp','password_reset','sign_in_link','verification')))), 0),
                    COALESCE(SUM(m.is_sent = 0
                                 AND m.display_tier = 'past_due'), 0),
                    COALESCE(SUM(m.is_sent = 0
                                 AND m.display_tier = 'deadline'), 0),
                    COALESCE(SUM(m.is_sent = 0
                                 AND m.display_tier = 'signal'), 0),
                    COALESCE(SUM(m.is_sent = 0
                                 AND m.display_tier = 'noise'), 0),
                    COALESCE(SUM(m.is_sent=0 AND m.display_tier='pending'),0)
             FROM classified m
             WHERE m.account_id = ?1 AND m.is_spam = 0
               AND m.received_at >= ?2 AND m.received_at < ?3
             GROUP BY day
             ORDER BY day"
        ))?;
        let count = |r: &rusqlite::Row<'_>, i: usize| -> rusqlite::Result<u64> {
            Ok(r.get::<_, i64>(i)?.max(0) as u64)
        };
        let rows = stmt
            .query_map(
                params![account_id, since.to_rfc3339(), until.to_rfc3339()],
                |r| {
                    Ok(MailActivityDay {
                        day: r.get(0)?,
                        received: count(r, 1)?,
                        sent: count(r, 2)?,
                        sealed: count(r, 3)?,
                        past_due: count(r, 4)?,
                        deadline: count(r, 5)?,
                        signal: count(r, 6)?,
                        noise: count(r, 7)?,
                        pending: count(r, 8)?,
                    })
                },
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}

/// The moment an account's open ledger started running.
///
/// The migration's stamp when there is one (see
/// `migrate::stamp_open_ledger_start`), and the ACCOUNT'S OWN `created_at` when
/// there is not: an account made after the column existed has been recording
/// since it existed, which is exactly what that says.
///
/// EVERY FAILURE FALLS TOWARD "NO EVIDENCE YET". A stamp that will not parse,
/// or an account row that is not there, resolves to NOW rather than to the
/// epoch — because the epoch reads as "the ledger has always been running",
/// which is the one answer that produces the flattering number this whole
/// mechanism exists to prevent.
pub(super) fn open_ledger_since(conn: &Connection, account_id: AccountId) -> Result<DateTime<Utc>> {
    ledger_since(conn, account_id, OPEN_LEDGER_SINCE_KEY)
}

/// The moment an account's DWELL ledger started running: the same resolution
/// as [`open_ledger_since`] over the other key, with the same fallbacks and
/// the same direction of failure (see `migrate::stamp_dwell_ledger_start`).
/// It floors `opened_at`, not `received_at`: mail that arrived last year and
/// was opened this morning was opened where dwell could be reported.
pub(super) fn dwell_ledger_since(
    conn: &Connection,
    account_id: AccountId,
) -> Result<DateTime<Utc>> {
    ledger_since(conn, account_id, DWELL_LEDGER_SINCE_KEY)
}

fn ledger_since(conn: &Connection, account_id: AccountId, key: &str) -> Result<DateTime<Utc>> {
    let stored: Option<String> = conn
        .query_row(
            "SELECT value FROM app_settings WHERE account_id = ?1 AND key = ?2",
            params![account_id, key],
            |r| r.get(0),
        )
        .optional()?;
    let stored = match stored {
        Some(s) => Some(s),
        None => conn
            .query_row(
                "SELECT created_at FROM accounts WHERE id = ?1",
                params![account_id],
                |r| r.get::<_, String>(0),
            )
            .optional()?,
    };
    Ok(stored
        .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
        .map(|d| d.with_timezone(&Utc))
        .unwrap_or_else(Utc::now))
}
