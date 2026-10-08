//! Recipient autocomplete over the Sent-derived contacts table, plus the
//! Sent-history harvest's merge. Search is HUMAN-DOOR ONLY (`/client/contacts`);
//! the agent door never learns the table exists.
//!
//! Also here, and human-door only for the same reason: `people_named`, the
//! calendar event pane's guest suggestion (`/client/calendar/people`).

use super::ghosts::{ELIGIBLE, ELIGIBLE_JOINS};
use super::*;
use std::collections::HashSet;

/// Name-matched people one `people_named` lookup probes `messages` for, taken
/// most recently written to first. Each probe is an index seek over one
/// sender's mail, under the store's one connection mutex.
const PEOPLE_PROBE_CAP: usize = 50;

/// One person's inbound mail that may speak for them: not sent, not spam, NOT
/// SEALED by the test calendar ghosts use ([`ELIGIBLE`]: the access review
/// allowed it and the stored decision names no auth kind), not sealed by the
/// retired `triage.sensitivity` column either (rows from before the agent
/// pipeline), and only mail that PASSED sender authentication (`auth_pass =
/// 1`: a NULL reads as no, the column's own rule in schema.sql).
///
/// `INDEXED BY` on purpose, as in `rules::SENDER_RULE_DIGEST_SQL`: without
/// statistics the planner may walk the whole account instead of one sender.
const PEOPLE_PROBE_FROM: &str = "FROM messages m INDEXED BY idx_messages_from_nocase";

fn people_probe_where() -> String {
    format!(
        "{ELIGIBLE_JOINS}
         LEFT JOIN triage t ON t.message_id = m.id AND t.account_id = m.account_id
         WHERE m.account_id = ?1
           AND m.from_addr = ?2 COLLATE NOCASE
           AND {ELIGIBLE}
           AND m.auth_pass = 1
           AND COALESCE(t.sensitivity, 'normal') != 'sealed'"
    )
}

/// WHEN a person last wrote, for somebody whose name the user's own Sent mail
/// already gave: one aggregate, NO SORT. (It was `ORDER BY received_at DESC`
/// read for its first row, which put every one of the sender's messages
/// through a temp b-tree first: verify7 D1.)
fn people_last_received_sql() -> String {
    format!(
        "SELECT MAX(m.received_at) {PEOPLE_PROBE_FROM} {}",
        people_probe_where()
    )
}

/// The same mail row by row and in no order, for somebody with no stored
/// name: the newest received_at, the newest name, and whether any of it is
/// list mail are all found in one pass in Rust, so there is still no sort.
fn people_names_sql() -> String {
    format!(
        "SELECT m.received_at, m.from_name,
                m.list_unsubscribe IS NOT NULL AND m.list_unsubscribe != ''
         {PEOPLE_PROBE_FROM} {}",
        people_probe_where()
    )
}

/// A From name that is somebody speaking THROUGH an address, not the name of
/// the address: `"Steven Ng via Team" <team@googlegroups.com>`, `"Steven from
/// Acme" <support@acme.com>`, `"Steven Ng (via Slack)"`. A list or a help
/// desk the user once wrote to must not be offered as the person "Steven"
/// (verify7 D4).
fn list_shaped(name: &str) -> bool {
    let words = name_words(name);
    // After the first word: somebody whose given name is "Via" is a person.
    // Brackets are dropped by `name_words`, so "(via Slack)" is caught too.
    words
        .iter()
        .skip(1)
        .any(|w| matches!(w.as_str(), "via" | "from" | "at"))
}

/// A name as lowercase words, split on whitespace, with the punctuation around
/// each word dropped (`"Steven Ng"` in quotes, `Ng,`).
fn name_words(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|w| {
            w.trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase()
        })
        .filter(|w| !w.is_empty())
        .collect()
}

/// THE MATCHING RULE: each typed word is a prefix of the name's word in the
/// same position, counted from the name's FIRST word. So "ste", "steven" and
/// "steven n" find "Steven Ng"; "ng" (not the given name) and "even" (not a
/// prefix) do not.
fn words_match(query: &[String], name: &str) -> bool {
    let name = name_words(name);
    !query.is_empty()
        && query.len() <= name.len()
        && query
            .iter()
            .zip(&name)
            .all(|(q, n)| n.starts_with(q.as_str()))
}

/// The match for a person with no name anywhere: ONE typed word, a prefix of
/// the first token of the address's local part (`steven.ng@x.com` is
/// "steven").
fn local_part_matches(query: &[String], addr: &str) -> bool {
    let [word] = query else { return false };
    let local = addr.split('@').next().unwrap_or_default();
    local
        .split(['.', '_', '-', '+'])
        .next()
        .is_some_and(|first| !first.is_empty() && first.to_lowercase().starts_with(word.as_str()))
}

fn lenient_dt(raw: Option<String>) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw?.trim())
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

impl SqliteStore {
    /// HUMAN-DOOR ONLY: rank contacts for a typed fragment. Prefix matches (on
    /// the address or the display name) sort above substring matches, then the
    /// user's OWN address, then by how often and how recently the user has
    /// written to them.
    ///
    /// Self is ranked WITHIN the tier the fragment earned, never above it: for
    /// your own address you are the answer and not the person you write to
    /// most, but a fragment that prefix-matches somebody else and merely
    /// appears somewhere inside your address still belongs to them.
    pub fn search_contacts(
        &self,
        account_id: AccountId,
        q: &str,
        limit: u32,
    ) -> Result<Vec<ContactEntry>> {
        let q = q.trim();
        if q.is_empty() {
            return Ok(Vec::new());
        }
        // LIKE metacharacters in the fragment are literal text to the user.
        let escaped = q
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        let contains = format!("%{escaped}%");
        let prefix = format!("{escaped}%");

        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT addr, display_name, sent_count, last_sent_at FROM contacts
             WHERE account_id = ?1
               AND (addr LIKE ?2 ESCAPE '\\' OR display_name LIKE ?2 ESCAPE '\\')
             ORDER BY (addr LIKE ?3 ESCAPE '\\' OR display_name LIKE ?3 ESCAPE '\\') DESC,
                      (lower(addr) = (SELECT lower(trim(email)) FROM accounts
                                      WHERE id = ?1)) DESC,
                      sent_count DESC,
                      COALESCE(last_sent_at, '') DESC,
                      addr ASC
             LIMIT ?4",
        )?;
        let rows = stmt
            .query_map(params![account_id, contains, prefix, limit], |r| {
                Ok(ContactEntry {
                    addr: r.get(0)?,
                    display_name: r.get(1)?,
                    sent_count: r.get(2)?,
                    last_sent_at: dt_opt(r, 3)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// HUMAN-DOOR ONLY: the people the user has WRITTEN TO whose name starts
    /// with the typed words, the most recently exchanged-with first. Behind
    /// the calendar event pane's guest suggestion, so the answer is a possible
    /// invitee and the rules are about who may become one:
    ///
    /// * Only a `contacts` row with `sent_count > 0`. Mail TO the user makes
    ///   nobody a candidate, so a stranger cannot become a suggested guest by
    ///   writing under a matching name. Never the account's own address.
    /// * The name is the one the user's own Sent mail used
    ///   (`contacts.display_name`). Without one, the newest name on that
    ///   address's own readable mail ([`people_names_sql`]); without that, the
    ///   address's local part, and the answer carries no name.
    /// * An address with no stored name whose readable mail is LIST-SHAPED is
    ///   not offered at all: any of it carries `List-Unsubscribe`, or a From
    ///   name that is somebody speaking through the address
    ///   ([`list_shaped`]: "Steven Ng via Team", "Steven from Acme"). A group
    ///   or a help desk the user once wrote to is not the person "Steven".
    /// * Order: two-way first, by the OLDER of the last mail each way (an
    ///   exchange is as recent as its older half); then people only written
    ///   to, by when; ties by address.
    ///
    /// The contacts rows are matched in Rust (a few thousand at most). There
    /// is no GROUP BY over `messages` (see `senders.rs`): each surviving
    /// person costs one probe on `idx_messages_from_nocase`, and at most
    /// [`PEOPLE_PROBE_CAP`] are probed. A person with no stored name can only
    /// be matched by reading their mail, which the cap forbids doing for every
    /// contact, so the sender directory picks which of them are worth a probe
    /// (its newest name matches, or the local part does) and the probe decides.
    pub fn people_named(
        &self,
        account_id: AccountId,
        name: &str,
        limit: u32,
    ) -> Result<Vec<PersonNamed>> {
        let query = name_words(name);
        if query.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let conn = self.lock()?;
        let own: Option<String> = conn
            .query_row(
                "SELECT lower(trim(email)) FROM accounts WHERE id = ?1",
                params![account_id],
                |r| r.get(0),
            )
            .optional()?;

        // A hint only: which addresses' newest inbound name matches. Never the
        // name served, and never what makes somebody a candidate.
        let mut hinted: HashSet<String> = HashSet::new();
        {
            let mut stmt = conn.prepare(
                "SELECT addr, display_name FROM senders
                 WHERE account_id = ?1 AND display_name IS NOT NULL",
            )?;
            let mut rows = stmt.query(params![account_id])?;
            while let Some(r) = rows.next()? {
                if words_match(&query, r.get_ref(1)?.as_str().unwrap_or_default()) {
                    hinted.insert(r.get::<_, String>(0)?.trim().to_lowercase());
                }
            }
        }

        struct Candidate {
            addr: String,
            name: Option<String>,
            last_sent: Option<DateTime<Utc>>,
            last_received: Option<DateTime<Utc>>,
        }
        let mut candidates: Vec<Candidate> = Vec::new();
        {
            let mut seen: HashSet<String> = HashSet::new();
            let mut stmt = conn.prepare(
                "SELECT addr, display_name, last_sent_at FROM contacts
                 WHERE account_id = ?1 AND sent_count > 0
                 ORDER BY COALESCE(last_sent_at, '') DESC, addr ASC",
            )?;
            let mut rows = stmt.query(params![account_id])?;
            while let Some(r) = rows.next()? {
                if candidates.len() >= PEOPLE_PROBE_CAP {
                    break;
                }
                let addr = r.get::<_, String>(0)?.trim().to_string();
                let key = addr.to_lowercase();
                if key.is_empty() || own.as_deref() == Some(key.as_str()) {
                    continue;
                }
                let stored = r
                    .get::<_, Option<String>>(1)?
                    .map(|n| n.trim().to_string())
                    .filter(|n| !n.is_empty());
                let worth_a_look = match &stored {
                    Some(n) => words_match(&query, n),
                    None => hinted.contains(&key) || local_part_matches(&query, &addr),
                };
                // The same address in two spellings is one person: the more
                // recently written-to row speaks for it.
                if !worth_a_look || !seen.insert(key) {
                    continue;
                }
                candidates.push(Candidate {
                    addr,
                    name: stored,
                    last_sent: lenient_dt(r.get(2)?),
                    last_received: None,
                });
            }
        }

        let mut last_received = conn.prepare(&people_last_received_sql())?;
        let mut their_names = conn.prepare(&people_names_sql())?;
        let mut people: Vec<Candidate> = Vec::new();
        for mut c in candidates {
            if c.name.is_some() {
                // Named by the user's own Sent mail: only when they last
                // wrote is wanted, and their mail's names and shape are never
                // read.
                let newest: Option<String> =
                    last_received.query_row(params![account_id, c.addr], |r| r.get(0))?;
                c.last_received = lenient_dt(newest);
                people.push(c);
                continue;
            }
            // NO STORED NAME: the newest name on their own readable mail
            // speaks for them, unless that mail is a list's or a desk's.
            // Compared as text, as the ORDER BY this replaces compared them.
            let mut newest: Option<String> = None;
            let mut newest_named: Option<(String, String)> = None;
            let mut relayed = false;
            let mut rows = their_names.query(params![account_id, c.addr])?;
            while let Some(r) = rows.next()? {
                let at = r.get::<_, Option<String>>(0)?.unwrap_or_default();
                let theirs = r
                    .get::<_, Option<String>>(1)?
                    .map(|n| n.trim().to_string())
                    .filter(|n| !n.is_empty());
                if r.get::<_, bool>(2)? || theirs.as_deref().is_some_and(list_shaped) {
                    relayed = true;
                }
                if let Some(name) = theirs
                    && newest_named.as_ref().is_none_or(|(when, _)| at > *when)
                {
                    newest_named = Some((at.clone(), name));
                }
                if newest.as_ref().is_none_or(|when| at > *when) {
                    newest = Some(at);
                }
            }
            drop(rows);
            // An address that carries other people's names (a list, a relay,
            // a help desk) is offered by the name the user's own Sent mail
            // gave it, or not at all: here there is none.
            if relayed {
                continue;
            }
            c.last_received = lenient_dt(newest);
            match newest_named {
                Some((_, n)) if words_match(&query, &n) => c.name = Some(n),
                // They have a name, and it is not the one typed.
                Some(_) => continue,
                None if local_part_matches(&query, &c.addr) => {}
                None => continue,
            }
            people.push(c);
        }

        // Two-way sorts on the older half of the exchange, above everybody
        // only written to, who sort on when. `None` is below every instant.
        let rank = |c: &Candidate| match c.last_sent.zip(c.last_received) {
            Some((sent, received)) => (Some(sent.min(received)), None),
            None => (None, c.last_sent),
        };
        people.sort_by(|a, b| rank(b).cmp(&rank(a)).then(a.addr.cmp(&b.addr)));
        people.truncate(limit as usize);
        Ok(people
            .into_iter()
            .map(|c| PersonNamed {
                addr: c.addr,
                display_name: c.name,
            })
            .collect())
    }

    /// Merge the Sent-history harvest's aggregate. MAX/COALESCE semantics, not
    /// increments: the harvest overlaps mail the ingest path already counted,
    /// and a re-run after an interrupted pass must be idempotent.
    pub fn merge_harvested_contacts(
        &self,
        account_id: AccountId,
        batch: &[ContactEntry],
    ) -> Result<()> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        for c in batch {
            if c.addr.trim().is_empty() {
                continue;
            }
            let last_sent = c.last_sent_at.map(|d| d.to_rfc3339());
            tx.execute(
                "INSERT INTO contacts(account_id, addr, sent_count, first_seen,
                                      last_sent_at, display_name)
                 VALUES(?1,?2,?3,COALESCE(?4,''),?4,?5)
                 ON CONFLICT(account_id, addr) DO UPDATE SET
                     sent_count = MAX(sent_count, excluded.sent_count),
                     last_sent_at = NULLIF(
                         MAX(COALESCE(last_sent_at,''), COALESCE(excluded.last_sent_at,'')), ''),
                     display_name = COALESCE(excluded.display_name, display_name)",
                params![account_id, c.addr, c.sent_count, last_sent, c.display_name],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone as _;

    fn entry(addr: &str, name: Option<&str>, count: i64, day: u32) -> ContactEntry {
        ContactEntry {
            addr: addr.into(),
            display_name: name.map(Into::into),
            sent_count: count,
            last_sent_at: Some(Utc.with_ymd_and_hms(2026, 7, day, 12, 0, 0).unwrap()),
        }
    }

    fn store_with(entries: &[ContactEntry]) -> (SqliteStore, AccountId) {
        let store = SqliteStore::open_in_memory().unwrap();
        let acct = store.ensure_account("me@example.com").unwrap();
        store.merge_harvested_contacts(acct, entries).unwrap();
        (store, acct)
    }

    #[test]
    fn prefix_beats_substring_then_count() {
        let (store, acct) = store_with(&[
            entry("zalice@x.com", None, 50, 1),
            entry("alice@x.com", Some("Alice"), 2, 1),
            entry("albert@x.com", None, 9, 1),
        ]);
        let hits = store.search_contacts(acct, "al", 10).unwrap();
        let addrs: Vec<_> = hits.iter().map(|h| h.addr.as_str()).collect();
        // Both prefix matches first (count-ordered), the substring match last.
        assert_eq!(addrs, ["albert@x.com", "alice@x.com", "zalice@x.com"]);
    }

    #[test]
    fn name_matches_too() {
        let (store, acct) = store_with(&[entry("a.j@x.com", Some("Alice Johnson"), 1, 1)]);
        let hits = store.search_contacts(acct, "johns", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].display_name.as_deref(), Some("Alice Johnson"));
    }

    #[test]
    fn like_metacharacters_are_literal() {
        let (store, acct) = store_with(&[entry("percent@x.com", None, 1, 1)]);
        assert!(store.search_contacts(acct, "%", 10).unwrap().is_empty());
        assert!(store.search_contacts(acct, "_", 10).unwrap().is_empty());
    }

    #[test]
    fn merge_is_idempotent_and_takes_max() {
        let (store, acct) = store_with(&[entry("bob@x.com", None, 3, 10)]);
        // Re-run with a lower count and older date: nothing regresses. A name
        // arriving later fills in.
        store
            .merge_harvested_contacts(acct, &[entry("bob@x.com", Some("Bob"), 1, 2)])
            .unwrap();
        let hits = store.search_contacts(acct, "bob", 10).unwrap();
        assert_eq!(hits[0].sent_count, 3);
        assert_eq!(hits[0].display_name.as_deref(), Some("Bob"));
        assert_eq!(
            hits[0].last_sent_at.unwrap(),
            Utc.with_ymd_and_hms(2026, 7, 10, 12, 0, 0).unwrap()
        );
    }

    #[test]
    fn your_own_address_is_a_contact_you_never_had_to_earn() {
        // Nothing in the mail path seeds it — both the Sent seed and the harvest
        // strip self — so `ensure_account` is the only thing standing between
        // the user and being a stranger to their own inbox.
        let (store, acct) = store_with(&[]);
        assert!(store.is_known_contact(acct, "me@example.com").unwrap());
        assert!(
            store.is_known_contact(acct, "ME@Example.COM").unwrap(),
            "the header spells it however it likes"
        );
        let hits = store.search_contacts(acct, "me@ex", 10).unwrap();
        assert_eq!(hits[0].addr, "me@example.com");
        assert_eq!(hits[0].display_name.as_deref(), Some("Me"));
    }

    #[test]
    fn typing_me_offers_yourself_first() {
        // The display name earns its keep here: "me" is how a person reaches for
        // their own address, and it outranks the contact they write to most.
        let (store, acct) = store_with(&[entry("mel@x.com", Some("Mel"), 99, 1)]);
        let hits = store.search_contacts(acct, "me", 10).unwrap();
        assert_eq!(hits[0].addr, "me@example.com");
        assert_eq!(hits[1].addr, "mel@x.com");
    }

    #[test]
    fn self_ranks_inside_its_tier_not_above_it() {
        // "exa" prefix-matches this contact and only appears INSIDE the user's
        // own address. The fragment picked its owner; self does not jump it.
        let (store, acct) = store_with(&[entry("example@x.com", None, 1, 1)]);
        let addrs: Vec<_> = store
            .search_contacts(acct, "exa", 10)
            .unwrap()
            .into_iter()
            .map(|h| h.addr)
            .collect();
        assert_eq!(addrs, ["example@x.com", "me@example.com"]);
    }

    #[test]
    fn reseeding_self_never_stomps_the_row() {
        // `ensure_account` runs on every daemon start. Suppose the row has moved
        // since (a name the user set, a count real mail bumped): a restart must
        // leave it exactly where it is, and must not double it.
        let (store, acct) = store_with(&[]);
        store
            .merge_harvested_contacts(acct, &[entry("me@example.com", Some("Braelyn"), 7, 11)])
            .unwrap();
        assert_eq!(store.ensure_account("me@example.com").unwrap(), acct);
        let hits = store.search_contacts(acct, "me@example.com", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].sent_count, 7);
        assert_eq!(hits[0].display_name.as_deref(), Some("Braelyn"));
    }

    // --- people_named ------------------------------------------------------

    fn days_ago(days: i64) -> DateTime<Utc> {
        Utc::now() - chrono::Duration::days(days)
    }

    /// A person the user wrote to `sent_days_ago`.
    fn wrote_to(addr: &str, name: Option<&str>, sent_days_ago: i64) -> ContactEntry {
        ContactEntry {
            addr: addr.into(),
            display_name: name.map(Into::into),
            sent_count: 1,
            last_sent_at: Some(days_ago(sent_days_ago)),
        }
    }

    /// Inbound mail from `from` as production leaves ordinary mail: a triage
    /// row at `sensitivity='normal'`, an access review that ALLOWED it, and no
    /// authentication failure. Returns the message id for a test to spoil.
    fn inbound(
        store: &SqliteStore,
        acct: AccountId,
        from: &str,
        from_name: Option<&str>,
        received_days_ago: i64,
    ) -> i64 {
        let conn = store.lock().unwrap();
        conn.execute(
            "INSERT INTO messages(account_id,gmail_msg_id,thread_id,from_addr,from_name,
                                  subject,received_at,snippet,body,auth_pass)
             VALUES(?1,lower(hex(randomblob(8))),'t',?2,?3,'hello',?4,'','',1)",
            params![
                acct,
                from,
                from_name,
                days_ago(received_days_ago).to_rfc3339()
            ],
        )
        .unwrap();
        let id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO triage(message_id,account_id,sensitivity,created_at)
             VALUES(?1,?2,'normal',?3)",
            params![id, acct, Utc::now().to_rfc3339()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO agent_message_state(account_id,message_id,content_snapshot,access)
             VALUES(?1,?2,'','allowed')",
            params![acct, id],
        )
        .unwrap();
        id
    }

    /// The sender directory's newest name for `addr`, as ingest leaves it:
    /// what makes a contact with no stored name worth a probe.
    fn seen_as(store: &SqliteStore, acct: AccountId, addr: &str, name: &str) {
        store
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO senders(account_id,addr,display_name,msg_count,first_seen,
                                     last_received_at)
                 VALUES(?1,?2,?3,1,?4,?4)
                 ON CONFLICT DO UPDATE SET display_name=excluded.display_name",
                params![acct, addr, name, Utc::now().to_rfc3339()],
            )
            .unwrap();
    }

    fn spoil(store: &SqliteStore, sql: &str, id: i64) {
        assert_eq!(store.lock().unwrap().execute(sql, params![id]).unwrap(), 1);
    }

    fn named(store: &SqliteStore, acct: AccountId, name: &str) -> Vec<String> {
        store
            .people_named(acct, name, 3)
            .unwrap()
            .into_iter()
            .map(|p| p.addr)
            .collect()
    }

    #[test]
    fn a_given_name_prefix_matches_and_a_later_word_or_a_middle_does_not() {
        let (store, acct) = store_with(&[wrote_to("sng@x.com", Some("Steven Ng"), 1)]);
        for hit in ["ste", "steven", "STEVEN", "steven n", "  Steven   Ng ", "s"] {
            assert_eq!(named(&store, acct, hit), ["sng@x.com"], "{hit:?}");
        }
        for miss in ["ng", "even", "steven x", "steven ng jr", "stevenn", ""] {
            assert!(named(&store, acct, miss).is_empty(), "{miss:?}");
        }
        assert_eq!(
            store.people_named(acct, "steven", 3).unwrap(),
            [PersonNamed {
                addr: "sng@x.com".into(),
                display_name: Some("Steven Ng".into()),
            }]
        );
    }

    #[test]
    fn two_way_beats_somebody_written_to_more_recently() {
        let (store, acct) = store_with(&[
            wrote_to("oneway@x.com", Some("Steven One"), 1),
            wrote_to("twoway@x.com", Some("Steven Two"), 40),
        ]);
        // Before any mail comes back, the more recently written-to is first.
        assert_eq!(
            named(&store, acct, "steven"),
            ["oneway@x.com", "twoway@x.com"]
        );
        inbound(&store, acct, "TwoWay@x.com", Some("Steven Two"), 30);
        assert_eq!(
            named(&store, acct, "steven"),
            ["twoway@x.com", "oneway@x.com"]
        );
    }

    #[test]
    fn an_exchange_is_as_recent_as_its_older_half() {
        let (store, acct) = store_with(&[
            wrote_to("a@x.com", Some("Steven A"), 1),
            wrote_to("b@x.com", Some("Steven B"), 7),
        ]);
        inbound(&store, acct, "a@x.com", None, 730);
        inbound(&store, acct, "b@x.com", None, 6);
        // A was written to yesterday but last answered two years ago; B both
        // ways last week. And the other half: C heard from yesterday, written
        // to a year ago.
        assert_eq!(named(&store, acct, "steven"), ["b@x.com", "a@x.com"]);
        store
            .merge_harvested_contacts(acct, &[wrote_to("c@x.com", Some("Steven C"), 365)])
            .unwrap();
        inbound(&store, acct, "c@x.com", None, 1);
        assert_eq!(
            named(&store, acct, "steven"),
            ["b@x.com", "c@x.com", "a@x.com"]
        );
    }

    #[test]
    fn somebody_who_only_wrote_to_the_user_is_never_offered() {
        let (store, acct) = store_with(&[]);
        inbound(
            &store,
            acct,
            "stranger@evil.test",
            Some("Steven Stranger"),
            1,
        );
        // A contacts row the user never sent to (the table allows one).
        store
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO contacts(account_id,addr,sent_count,first_seen,display_name)
                 VALUES(?1,'steven.zero@evil.test',0,'2026-01-01','Steven Zero')",
                params![acct],
            )
            .unwrap();
        inbound(
            &store,
            acct,
            "steven.zero@evil.test",
            Some("Steven Zero"),
            1,
        );
        assert!(named(&store, acct, "steven").is_empty());
        // The same row once the user has written to it is a candidate.
        store
            .lock()
            .unwrap()
            .execute(
                "UPDATE contacts SET sent_count=1 WHERE addr='steven.zero@evil.test'",
                [],
            )
            .unwrap();
        assert_eq!(named(&store, acct, "steven"), ["steven.zero@evil.test"]);
    }

    /// verify7 D4, the reviewer's cases. Each address has been written to (so
    /// it is a candidate) and has NO stored name (ingest seeds contacts
    /// without one), and its newest inbound From name starts with "Steven".
    #[test]
    fn a_list_or_a_desk_is_not_offered_under_the_name_of_whoever_posted() {
        let relays = [
            ("team@googlegroups.com", "Steven Ng via Team"),
            ("support@acme.com", "Steven from Acme"),
            ("notify@slack.test", "Steven Ng (via Slack)"),
            ("hello@shop.test", "Steven at Shop"),
        ];
        for (addr, name) in relays {
            let (store, acct) =
                store_with(&[wrote_to(addr, None, 1), wrote_to("real@x.com", None, 30)]);
            inbound(&store, acct, addr, Some(name), 0);
            seen_as(&store, acct, addr, name);
            inbound(&store, acct, "real@x.com", Some("Steven Real"), 20);
            seen_as(&store, acct, "real@x.com", "Steven Real");
            assert_eq!(named(&store, acct, "steven"), ["real@x.com"], "{name:?}");
        }
        // An OLDER list-shaped name is enough: the address relays, whoever
        // posted last.
        let (store, acct) = store_with(&[wrote_to("team@googlegroups.com", None, 1)]);
        inbound(
            &store,
            acct,
            "team@googlegroups.com",
            Some("Ana via Team"),
            9,
        );
        inbound(&store, acct, "team@googlegroups.com", Some("Steven Ng"), 0);
        seen_as(&store, acct, "team@googlegroups.com", "Steven Ng");
        assert!(named(&store, acct, "steven").is_empty());
        // A bulk sender, known by its List-Unsubscribe header, under a plain
        // name.
        let (store, acct) = store_with(&[wrote_to("news@list.test", None, 1)]);
        let id = inbound(&store, acct, "news@list.test", Some("Steven Ng"), 0);
        seen_as(&store, acct, "news@list.test", "Steven Ng");
        assert_eq!(named(&store, acct, "steven"), ["news@list.test"]);
        spoil(
            &store,
            "UPDATE messages SET list_unsubscribe='<mailto:u@list.test>' WHERE id=?1",
            id,
        );
        assert!(named(&store, acct, "steven").is_empty());
        // And not by its local part either: "or not at all".
        let (store, acct) = store_with(&[wrote_to("steven@lists.test", None, 1)]);
        inbound(&store, acct, "steven@lists.test", Some("Ana via Steven"), 0);
        assert!(named(&store, acct, "steven").is_empty());
        // The name the user's OWN Sent mail gave the address still stands.
        let (store, acct) =
            store_with(&[wrote_to("team@googlegroups.com", Some("Steven's Team"), 1)]);
        inbound(
            &store,
            acct,
            "team@googlegroups.com",
            Some("Ana via Team"),
            0,
        );
        assert_eq!(named(&store, acct, "steven"), ["team@googlegroups.com"]);
        // Mail that does not count (spam here) does not make a relay of
        // anybody: an attacker cannot hide a real contact by forging a list.
        let (store, acct) = store_with(&[wrote_to("real@x.com", None, 1)]);
        inbound(&store, acct, "real@x.com", Some("Steven Real"), 3);
        seen_as(&store, acct, "real@x.com", "Steven Real");
        let forged = inbound(&store, acct, "real@x.com", Some("Steven via List"), 0);
        spoil(
            &store,
            "UPDATE messages SET auth_pass=0 WHERE id=?1",
            forged,
        );
        assert_eq!(named(&store, acct, "steven"), ["real@x.com"]);
    }

    #[test]
    fn list_shapes() {
        for relayed in [
            "Steven Ng via Team",
            "Steven from Acme",
            "Steven Ng (via Slack)",
            "\"Steven Ng\" VIA Team",
            "Steven at Shop",
        ] {
            assert!(list_shaped(relayed), "{relayed:?}");
        }
        for person in [
            "Steven Ng",
            "Via Ferrata",
            "From Russia",
            "Steven Viator",
            "Steven",
        ] {
            assert!(!list_shaped(person), "{person:?}");
        }
    }

    /// verify7 D1: neither probe sorts. The old one read `ORDER BY
    /// received_at DESC` for its first row, a temp b-tree over every message
    /// the sender ever sent.
    #[test]
    fn the_probes_seek_one_sender_and_never_sort() {
        let (store, acct) = store_with(&[]);
        let conn = store.lock().unwrap();
        for sql in [people_last_received_sql(), people_names_sql()] {
            let plan: Vec<String> = conn
                .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
                .unwrap()
                .query_map(params![acct, "a@x.com"], |r| r.get::<_, String>(3))
                .unwrap()
                .collect::<std::result::Result<_, _>>()
                .unwrap();
            let plan = plan.join(" | ");
            assert!(plan.contains("idx_messages_from_nocase"), "{plan}");
            assert!(!plan.contains("TEMP B-TREE"), "{plan}");
            assert!(!plan.contains("SCAN m"), "{plan}");
            assert!(!sql.contains("ORDER BY"), "{sql}");
        }
    }

    /// The newest name and the newest mail are still the newest, with no
    /// ORDER BY to say so: rows inserted oldest-last.
    #[test]
    fn the_newest_name_and_the_newest_mail_are_found_without_a_sort() {
        let (store, acct) = store_with(&[
            wrote_to("s@x.com", None, 1),
            wrote_to("other@x.com", Some("Steven Other"), 3),
        ]);
        inbound(&store, acct, "s@x.com", Some("Steven New"), 5);
        seen_as(&store, acct, "s@x.com", "Steven New");
        inbound(&store, acct, "s@x.com", Some("Bob Old"), 40);
        inbound(&store, acct, "s@x.com", None, 1);
        inbound(&store, acct, "other@x.com", Some("x"), 2);
        inbound(&store, acct, "other@x.com", Some("x"), 60);
        let people = store.people_named(acct, "steven", 3).unwrap();
        // s: sent 1 day ago, last received 1 day ago (the nameless newest
        // mail, the LAST row read, not the first): older half 1. other: sent
        // 3, last received 2: older half 3. Were s's first row (5 days ago)
        // taken for its newest, other would come first.
        assert_eq!(
            people,
            [
                PersonNamed {
                    addr: "s@x.com".into(),
                    display_name: Some("Steven New".into()),
                },
                PersonNamed {
                    addr: "other@x.com".into(),
                    display_name: Some("Steven Other".into()),
                },
            ]
        );
        // The older name is not the one typed, even though it is there.
        assert!(named(&store, acct, "bob").is_empty());
    }

    /// For somebody with a stored name the aggregate is the NEWEST mail, not
    /// the oldest and not the first inserted.
    #[test]
    fn a_named_persons_last_mail_is_their_newest() {
        let (store, acct) = store_with(&[
            wrote_to("a@x.com", Some("Steven A"), 1),
            wrote_to("b@x.com", Some("Steven B"), 1),
        ]);
        inbound(&store, acct, "a@x.com", None, 50);
        inbound(&store, acct, "a@x.com", None, 2);
        inbound(&store, acct, "a@x.com", None, 30);
        inbound(&store, acct, "b@x.com", None, 10);
        // a: older half 2. b: older half 10. By a's oldest mail (50) b would
        // lead.
        assert_eq!(named(&store, acct, "steven"), ["a@x.com", "b@x.com"]);
    }

    #[test]
    fn the_accounts_own_address_is_never_offered() {
        // `ensure_account` seeds self as a contact named "Me"; give it the
        // typed name, in a second spelling too.
        let (store, acct) = store_with(&[
            wrote_to("me@example.com", Some("Steven Self"), 1),
            wrote_to("Me@Example.COM", Some("Steven Self"), 1),
            wrote_to("other@x.com", Some("Steven Other"), 9),
        ]);
        assert_eq!(named(&store, acct, "steven"), ["other@x.com"]);
        // And under the name it is seeded with.
        let (store, acct) = store_with(&[]);
        assert!(named(&store, acct, "me").is_empty());
    }

    /// The fixture every "this mail does not count" test shares: `quiet` was
    /// written to recently and never answered; `loud` was written to earlier
    /// and has one inbound message. While that message counts, `loud` is
    /// two-way and first; each test proves that, spoils it, and sees the order
    /// flip.
    fn loud_and_quiet() -> (SqliteStore, AccountId, i64) {
        let (store, acct) = store_with(&[
            wrote_to("quiet@x.com", Some("Steven Quiet"), 2),
            wrote_to("loud@x.com", Some("Steven Loud"), 20),
        ]);
        let id = inbound(&store, acct, "loud@x.com", Some("Steven Loud"), 1);
        assert_eq!(
            named(&store, acct, "steven"),
            ["loud@x.com", "quiet@x.com"],
            "the fixture's mail counts until it is spoiled"
        );
        (store, acct, id)
    }
    const ONE_WAY: [&str; 2] = ["quiet@x.com", "loud@x.com"];

    #[test]
    fn spam_is_not_an_exchange() {
        let (store, acct, id) = loud_and_quiet();
        spoil(&store, "UPDATE messages SET is_spam=1 WHERE id=?1", id);
        assert_eq!(named(&store, acct, "steven"), ONE_WAY);
    }

    #[test]
    fn the_users_own_sent_mail_is_not_mail_from_them() {
        let (store, acct, id) = loud_and_quiet();
        spoil(&store, "UPDATE messages SET is_sent=1 WHERE id=?1", id);
        assert_eq!(named(&store, acct, "steven"), ONE_WAY);
    }

    #[test]
    fn mail_that_failed_authentication_is_not_an_exchange() {
        let (store, acct, id) = loud_and_quiet();
        spoil(&store, "UPDATE messages SET auth_pass=0 WHERE id=?1", id);
        assert_eq!(named(&store, acct, "steven"), ONE_WAY);
        // Never evaluated reads as no, as everywhere the column is read.
        spoil(&store, "UPDATE messages SET auth_pass=NULL WHERE id=?1", id);
        assert_eq!(named(&store, acct, "steven"), ONE_WAY);
        spoil(&store, "UPDATE messages SET auth_pass=1 WHERE id=?1", id);
        assert_eq!(named(&store, acct, "steven"), ["loud@x.com", "quiet@x.com"]);
    }

    /// Sealed as mail is sealed TODAY: the stored decision names an auth kind
    /// while `triage.sensitivity` still says `normal` and access is `allowed`.
    #[test]
    fn mail_with_an_auth_decision_is_not_an_exchange() {
        use crate::triage::decision::{AuthAssessment, AuthKind, MessageDecision};
        let (store, acct, id) = loud_and_quiet();
        let decision = |kinds: Vec<AuthKind>| {
            serde_json::to_string(&MessageDecision {
                auth: AuthAssessment {
                    kinds,
                    evidence: vec![],
                },
                ..Default::default()
            })
            .unwrap()
        };
        let store_decision = |json: String| {
            store
                .lock()
                .unwrap()
                .execute(
                    "INSERT INTO agent_message_decisions(account_id,message_id,revision,
                                                         decision_json,decided_at)
                     VALUES(?1,?2,1,?3,'2026-01-01T00:00:00Z')
                     ON CONFLICT(account_id,message_id) DO UPDATE
                         SET decision_json=excluded.decision_json",
                    params![acct, id, json],
                )
                .unwrap();
        };
        // A decision that names no auth kind seals nothing.
        store_decision(decision(vec![]));
        assert_eq!(named(&store, acct, "steven"), ["loud@x.com", "quiet@x.com"]);
        store_decision(decision(vec![AuthKind::LoginAlert]));
        let (sensitivity, access): (String, String) = store
            .lock()
            .unwrap()
            .query_row(
                "SELECT t.sensitivity, a.access FROM triage t
                 JOIN agent_message_state a ON a.message_id=t.message_id
                 WHERE t.message_id=?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            (sensitivity.as_str(), access.as_str()),
            ("normal", "allowed")
        );
        assert_eq!(named(&store, acct, "steven"), ONE_WAY);
        // A decision nobody can parse is not known to be harmless.
        store_decision("not json".into());
        assert_eq!(named(&store, acct, "steven"), ONE_WAY);
    }

    #[test]
    fn restricted_or_unreviewed_mail_is_not_an_exchange() {
        for access in ["restricted", "pending"] {
            let (store, acct, id) = loud_and_quiet();
            spoil(
                &store,
                &format!("UPDATE agent_message_state SET access='{access}' WHERE message_id=?1"),
                id,
            );
            assert_eq!(named(&store, acct, "steven"), ONE_WAY, "{access}");
        }
        // No review row at all is a review not yet done.
        let (store, acct, id) = loud_and_quiet();
        spoil(
            &store,
            "DELETE FROM agent_message_state WHERE message_id=?1",
            id,
        );
        assert_eq!(named(&store, acct, "steven"), ONE_WAY);
    }

    /// The retired column, on a row every current test would pass: access
    /// allowed, no auth decision.
    #[test]
    fn mail_sealed_by_the_legacy_column_is_not_an_exchange() {
        let (store, acct, id) = loud_and_quiet();
        spoil(
            &store,
            "UPDATE triage SET sensitivity='sealed' WHERE message_id=?1",
            id,
        );
        assert_eq!(named(&store, acct, "steven"), ONE_WAY);
    }

    #[test]
    fn people_named_is_capped() {
        let people: Vec<ContactEntry> = (1..=5)
            .map(|n| wrote_to(&format!("s{n}@x.com"), Some("Steven"), n))
            .collect();
        let (store, acct) = store_with(&people);
        assert_eq!(
            named(&store, acct, "steven"),
            ["s1@x.com", "s2@x.com", "s3@x.com"]
        );
        assert_eq!(store.people_named(acct, "steven", 1).unwrap().len(), 1);
        assert!(store.people_named(acct, "steven", 0).unwrap().is_empty());
    }

    #[test]
    fn only_the_most_recently_written_to_matches_are_probed() {
        // 51 Stevens; the one written to longest ago is the only two-way one,
        // and is past the probe cap, so it is not looked at.
        let mut people: Vec<ContactEntry> = (1..=PEOPLE_PROBE_CAP as i64)
            .map(|n| wrote_to(&format!("s{n:02}@x.com"), Some("Steven"), n))
            .collect();
        people.push(wrote_to("last@x.com", Some("Steven"), 400));
        let (store, acct) = store_with(&people);
        inbound(&store, acct, "last@x.com", None, 1);
        inbound(&store, acct, "s50@x.com", None, 1);
        assert_eq!(
            named(&store, acct, "steven"),
            ["s50@x.com", "s01@x.com", "s02@x.com"]
        );
    }

    #[test]
    fn the_name_the_user_used_wins_over_the_name_a_sender_chose() {
        let (store, acct) = store_with(&[wrote_to("bob@x.com", Some("Bob Jones"), 1)]);
        inbound(&store, acct, "bob@x.com", Some("Steven Totally"), 1);
        // The sender directory has the chosen name too, as ingest would.
        store
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO senders(account_id,addr,display_name,msg_count,first_seen,
                                     last_received_at)
                 VALUES(?1,'bob@x.com','Steven Totally',1,?2,?2)",
                params![acct, Utc::now().to_rfc3339()],
            )
            .unwrap();
        assert!(named(&store, acct, "steven").is_empty());
        assert_eq!(
            store.people_named(acct, "bob", 3).unwrap(),
            [PersonNamed {
                addr: "bob@x.com".into(),
                display_name: Some("Bob Jones".into()),
            }]
        );
    }

    #[test]
    fn with_no_stored_name_the_newest_name_on_their_readable_mail_is_used() {
        let (store, acct) = store_with(&[wrote_to("sp@x.com", None, 3)]);
        store
            .upsert_message(&NewMessage {
                account_id: acct,
                gmail_msg_id: "directory".into(),
                thread_id: "t".into(),
                from_addr: "SP@x.com".into(),
                from_name: Some("Steven Park".into()),
                subject: "hi".into(),
                received_at: days_ago(5),
                snippet: String::new(),
                body: String::new(),
                body_html: None,
                is_sent: false,
                is_spam: false,
                to_addrs: None,
                cc_addrs: None,
                bcc_addrs: None,
                list_unsubscribe: None,
                list_unsub_one_click: false,
                auth_pass: Some(true),
            })
            .unwrap();
        // The directory knows the name, but nothing readable carries it yet
        // (the ingested message has had no access review).
        assert!(named(&store, acct, "steven").is_empty());
        inbound(&store, acct, "sp@x.com", Some("Stevie"), 9);
        assert!(
            named(&store, acct, "steven").is_empty(),
            "their readable name is Stevie"
        );
        inbound(&store, acct, "sp@x.com", Some("Steven Park"), 4);
        inbound(&store, acct, "sp@x.com", Some("  "), 2);
        assert_eq!(
            store.people_named(acct, "steven p", 3).unwrap(),
            [PersonNamed {
                addr: "sp@x.com".into(),
                display_name: Some("Steven Park".into()),
            }]
        );
        // A newer name on mail that does not count changes nothing.
        let spam = inbound(&store, acct, "sp@x.com", Some("Robert"), 1);
        spoil(&store, "UPDATE messages SET is_spam=1 WHERE id=?1", spam);
        assert_eq!(named(&store, acct, "steven"), ["sp@x.com"]);
        // A newer readable one does: they are Robert now.
        spoil(&store, "UPDATE messages SET is_spam=0 WHERE id=?1", spam);
        assert!(named(&store, acct, "steven").is_empty());
    }

    #[test]
    fn with_no_name_anywhere_the_local_parts_first_token_matches() {
        let (store, acct) = store_with(&[
            wrote_to("steven.ng@x.com", None, 1),
            wrote_to("ng_steven@x.com", None, 1),
        ]);
        for hit in ["steven", "ste", "STEVEN"] {
            assert_eq!(
                store.people_named(acct, hit, 3).unwrap(),
                [PersonNamed {
                    addr: "steven.ng@x.com".into(),
                    display_name: None,
                }],
                "{hit:?}"
            );
        }
        assert_eq!(named(&store, acct, "ng"), ["ng_steven@x.com"]);
        for miss in ["even", "steven ng", "x"] {
            assert!(named(&store, acct, miss).is_empty(), "{miss:?}");
        }
        // Once their mail gives them a name, the address stops speaking.
        inbound(&store, acct, "steven.ng@x.com", Some("Robert"), 1);
        assert!(named(&store, acct, "steven").is_empty());
    }

    #[test]
    fn another_accounts_people_are_never_returned() {
        let (store, acct) = store_with(&[wrote_to("mine@x.com", Some("Steven Mine"), 5)]);
        let other = store.ensure_account("other@example.com").unwrap();
        store
            .merge_harvested_contacts(
                other,
                &[
                    wrote_to("theirs@x.com", Some("Steven Theirs"), 3),
                    wrote_to("mine@x.com", Some("Steven Mine"), 1),
                ],
            )
            .unwrap();
        // The other account's mail from a shared address makes nobody two-way
        // here, and its names name nobody here.
        inbound(&store, other, "theirs@x.com", Some("Steven Theirs"), 3);
        store
            .merge_harvested_contacts(acct, &[wrote_to("newer@x.com", Some("Steven Newer"), 2)])
            .unwrap();
        inbound(&store, other, "mine@x.com", Some("Steven Mine"), 1);
        assert_eq!(named(&store, acct, "steven"), ["newer@x.com", "mine@x.com"]);
        assert_eq!(
            named(&store, other, "steven"),
            ["mine@x.com", "theirs@x.com"]
        );
    }

    #[test]
    fn empty_query_returns_nothing() {
        let (store, acct) = store_with(&[entry("bob@x.com", None, 1, 1)]);
        assert!(store.search_contacts(acct, "  ", 10).unwrap().is_empty());
    }
}
