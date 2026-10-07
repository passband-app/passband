//! A correspondent's timezone, kept on their profile (`sender_zones`) and
//! served on the contact card. HUMAN-DOOR ONLY, like the rest of the card. The
//! inference itself is pure and lives in [`crate::timezone`]; this module
//! captures its inputs at ingest and stores its answer.
//!
//! LEARNED AT INGEST, and only when a message brings something to learn: a
//! non-zero `Date` offset from its sender, or an invitation naming a zone. Bulk
//! mail stamped UTC (nearly all of it) costs one UPDATE, one indexed DELETE
//! and two indexed reads of this message's own rows, and nothing else. When
//! there is something to learn, the sender's whole answer is RECOMPUTED from
//! the stored observations rather than adjusted, for the reason
//! `senders::bump_sender_conn` recomputes its count: a re-ingest of the same
//! message must not count it twice. The offsets read is a covering index
//! probe on the sender bounded by the inference's twelve-month window and its
//! cap on observations (see [`OFFSETS_SQL`]).
//!
//! ONLY AUTHENTICATED MAIL TEACHES ANYTHING (`messages.auth_pass = 1`; NULL,
//! never evaluated, reads as no). Anyone can put any From and any Date on a
//! message, and a zone inferred from forged mail would put the user's
//! suggested meeting times in the wrong place for a real person. An
//! invitation is BOUND to its sender as well: see [`invitation_is_bound`].
//!
//! A failure here never fails ingest (see [`guarded`]): a timezone is worth
//! less than the mail it rides on.

use super::*;
use crate::timezone::{
    FUTURE_TOLERANCE_DAYS, InferredZone, InviteZone, MAX_OFFSET_OBSERVATIONS, MAX_ORGANIZER_BYTES,
    OFFSET_WINDOW_DAYS, OffsetObservation, ZoneConfidence, ZoneSource, canonical_zone, infer_zone,
};

/// One sender's offset observations, newest first: authenticated, inbound,
/// not spam, inside the twelve-month window and not dated more than
/// [`FUTURE_TOLERANCE_DAYS`] ahead. The window and the cap are applied here
/// for cost and again in `infer_zone` as the rule; the future bound only
/// here, because what it protects is which rows fill the cap.
///
/// THE PARTIAL COVERING INDEX `idx_messages_zone_offsets` serves it (created
/// in `migrate`, because it names migrated columns): only rows with a
/// non-zero offset are in it, and it holds every column this query reads, so
/// the probe never visits the message row. That matters because the row's
/// later columns sit after the body, on its overflow pages, and this runs
/// inside ingest with the store mutex held. The index is keyed sender, then
/// the three flags this query pins to one value, then time, so a flood of
/// forged or spam mail in a real person's name is skipped by the seek rather
/// than walked, and the walk goes newest-first and stops at the cap. A
/// sender's UTC-stamped mail is never read at all. The
/// `date_offset_minutes != 0` term must stay spelled as the index's WHERE
/// is, or SQLite will not prove the index usable. `tests/zones.rs` pins the
/// plan.
pub(super) const OFFSETS_SQL: &str = "SELECT received_at, date_offset_minutes FROM messages
     WHERE account_id = ?1 AND from_addr = ?2 COLLATE NOCASE
       AND auth_pass = 1 AND is_sent = 0 AND is_spam = 0
       AND date_offset_minutes != 0
       AND received_at >= ?3 AND received_at <= ?4
     ORDER BY received_at DESC
     LIMIT ?5";

/// One organizer's invitation zones, newest first: the newest
/// [`crate::timezone::INVITE_WINDOW`] rows not dated past the future bound,
/// of which those whose message is inbound and not spam count (a spam or sent
/// message's invitation stays on file, unread, until a rescue makes it
/// count).
///
/// This read is a seek on `idx_sender_zone_invites_recent` (the address, then
/// the future bound as a range) that walks the index in this order and stops
/// at the window, so its cost does not grow with how many invitations the
/// person has ever sent and nothing is sorted. Only the window's rows are then
/// checked against `messages` ([`INVITE_COUNTS_SQL`], by primary key), whose
/// flags sit after the body. `tests/zones.rs` pins both plans.
pub(super) const INVITES_SQL: &str = "SELECT zone, message_id FROM sender_zone_invites
     WHERE account_id = ?1 AND addr = ?2 AND observed_at <= ?3
     ORDER BY observed_at DESC, message_id DESC
     LIMIT ?4";

/// Whether one invitation in the window counts: its message is inbound and
/// not spam.
pub(super) const INVITE_COUNTS_SQL: &str = "SELECT EXISTS(SELECT 1 FROM messages
     WHERE id = ?2 AND account_id = ?1 AND is_sent = 0 AND is_spam = 0)";

/// THE INVITATION BINDING. An `.ics` names its own organizer, and anyone can
/// write one: a forged invitation naming a real person as organizer would set
/// that person's zone at high confidence, above any number of their real
/// offsets. So an invitation is credited only when the message is
/// AUTHENTICATED and its From IS the organizer (compared case-insensitively,
/// as the addresses are stored).
///
/// Not "someone at the organizer's organisation": authentication vouches for
/// a domain, and telling an organisation's domain from one that strangers
/// share (a consumer ISP, a mail provider's hundred sign-up domains) takes a
/// list that is never complete. An invitation a delegate or a calendar
/// address sends is no observation; the organizer's own mail still teaches
/// their zone through its offsets.
fn invitation_is_bound(from_addr: &str, auth_pass: Option<i64>, organizer: &str) -> bool {
    auth_pass == Some(1) && from_addr.trim().eq_ignore_ascii_case(organizer.trim())
}

/// Is `addr` the account's own address? Its owner's zone is the device's,
/// and a row for it would let the composer "infer" the user from their own
/// mail echoed back. `person_record`'s `is_self` rule exactly.
fn is_own_address(conn: &Connection, account_id: AccountId, addr: &str) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT lower(trim(email)) = lower(trim(?2)) FROM accounts WHERE id = ?1",
            params![account_id, addr],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(false))
}

/// Recompute and store the inferred zone for one address from everything the
/// store holds about it. Never touches `user_zone`. An address inference has
/// no answer for keeps no row unless it carries an override.
pub(super) fn refresh_sender_zone_conn(
    conn: &Connection,
    account_id: AccountId,
    addr: &str,
    now: DateTime<Utc>,
) -> Result<()> {
    let addr = addr.trim().to_ascii_lowercase();
    if addr.is_empty() || is_own_address(conn, account_id, &addr)? {
        return Ok(());
    }

    // `now` is the wall clock at the recompute (every caller passes
    // `Utc::now()`), so the bound moves with time: a message dated next week
    // starts counting next week, as the ordinary mail it will then be.
    let until = (now + chrono::Duration::days(FUTURE_TOLERANCE_DAYS)).to_rfc3339();

    // The rows were bound to their sender when written (`observe`).
    let mut stmt = conn.prepare_cached(INVITES_SQL)?;
    let window = stmt
        .query_map(
            params![
                account_id,
                addr,
                until,
                crate::timezone::INVITE_WINDOW as i64
            ],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
        )?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut counts = conn.prepare_cached(INVITE_COUNTS_SQL)?;
    let mut invites = Vec::with_capacity(window.len());
    for (zone, message_id) in window {
        if counts.query_row(params![account_id, message_id], |r| r.get::<_, bool>(0))? {
            invites.push(zone);
        }
    }

    let since = (now - chrono::Duration::days(OFFSET_WINDOW_DAYS)).to_rfc3339();
    let mut stmt = conn.prepare_cached(OFFSETS_SQL)?;
    let offsets = stmt
        .query_map(
            params![
                account_id,
                addr,
                since,
                until,
                MAX_OFFSET_OBSERVATIONS as i64
            ],
            |r| {
                Ok(OffsetObservation {
                    at: dt(r, 0)?,
                    offset_minutes: r.get(1)?,
                })
            },
        )?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    let now_s = now.to_rfc3339();
    match infer_zone(None, &invites, &offsets, now) {
        Some(z) => {
            conn.execute(
                "INSERT INTO sender_zones(account_id, addr, zone, source, confidence, updated_at)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(account_id, addr) DO UPDATE SET
                     zone = excluded.zone, source = excluded.source,
                     confidence = excluded.confidence, updated_at = excluded.updated_at",
                params![
                    account_id,
                    addr,
                    z.zone,
                    z.source.as_str(),
                    z.confidence.as_str(),
                    now_s
                ],
            )?;
        }
        None => {
            conn.execute(
                "UPDATE sender_zones SET zone = NULL, source = NULL, confidence = NULL,
                        updated_at = ?3
                 WHERE account_id = ?1 AND addr = ?2",
                params![account_id, addr, now_s],
            )?;
            conn.execute(
                "DELETE FROM sender_zones
                 WHERE account_id = ?1 AND addr = ?2 AND zone IS NULL AND user_zone IS NULL",
                params![account_id, addr],
            )?;
        }
    }
    Ok(())
}

/// Recompute every address this stored message is an observation of: its
/// sender when it carries a non-zero offset, and each organizer of an
/// invitation it holds. Nothing for sent or spam mail, judged on the row's
/// own (sticky) flags, which are what the histogram reads.
fn refresh_for_message(
    conn: &Connection,
    account_id: AccountId,
    message_id: i64,
    now: DateTime<Utc>,
) -> Result<()> {
    let row: Option<(String, Option<i32>, bool, bool)> = conn
        .query_row(
            "SELECT from_addr, date_offset_minutes, is_sent, is_spam FROM messages
             WHERE account_id = ?1 AND id = ?2",
            params![account_id, message_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    let Some((from_addr, offset, is_sent, is_spam)) = row else {
        return Ok(());
    };
    if is_sent || is_spam {
        return Ok(());
    }
    let mut addrs: Vec<String> = Vec::new();
    if offset.is_some_and(|o| o != 0) {
        addrs.push(from_addr.trim().to_ascii_lowercase());
    }
    let mut stmt = conn.prepare_cached(
        "SELECT addr FROM sender_zone_invites WHERE account_id = ?1 AND message_id = ?2",
    )?;
    for organizer in stmt.query_map(params![account_id, message_id], |r| r.get::<_, String>(0))? {
        let organizer = organizer?.to_ascii_lowercase();
        if !addrs.contains(&organizer) {
            addrs.push(organizer);
        }
    }
    for addr in addrs {
        refresh_sender_zone_conn(conn, account_id, &addr, now)?;
    }
    Ok(())
}

/// Record one ingested message's observations and recompute whom they
/// concern, in the ingest transaction. The Date header's offset is already on
/// the row: the message upsert writes it (with the recipients' rule, `None`
/// keeping what is stored), so the row is written once. The invitation row
/// is REPLACED, so a re-ingest leaves exactly what this sighting says (and a
/// re-ingest that no longer authenticates removes it).
fn observe(
    conn: &Connection,
    account_id: AccountId,
    message_id: i64,
    invite: Option<&InviteZone>,
    now: DateTime<Utc>,
) -> Result<()> {
    conn.execute(
        "DELETE FROM sender_zone_invites WHERE account_id = ?1 AND message_id = ?2",
        params![account_id, message_id],
    )?;
    if let Some(invite) = invite {
        record_invite(conn, account_id, message_id, invite)?;
    }
    refresh_for_message(conn, account_id, message_id, now)
}

/// Write one invitation observation, or nothing. Nothing unless the message
/// is bound to the organizer ([`invitation_is_bound`]), the organizer is an
/// address of a sane length and not the account's own, and the zone is a
/// place; the zone is stored under its [`canonical_zone`] name for the
/// message's own year, so every later vote counts one clock once.
fn record_invite(
    conn: &Connection,
    account_id: AccountId,
    message_id: i64,
    invite: &InviteZone,
) -> Result<()> {
    let row: Option<(String, Option<i64>, DateTime<Utc>)> = conn
        .query_row(
            "SELECT from_addr, auth_pass, received_at FROM messages
             WHERE account_id = ?1 AND id = ?2",
            params![account_id, message_id],
            |r| Ok((r.get(0)?, r.get(1)?, dt(r, 2)?)),
        )
        .optional()?;
    let Some((from_addr, auth_pass, received_at)) = row else {
        return Ok(());
    };
    let organizer = invite.organizer.trim();
    if organizer.len() > MAX_ORGANIZER_BYTES
        || !invitation_is_bound(&from_addr, auth_pass, organizer)
        || is_own_address(conn, account_id, organizer)?
    {
        return Ok(());
    }
    let Some(zone) = canonical_zone(&invite.zone, received_at) else {
        return Ok(());
    };
    conn.execute(
        "INSERT INTO sender_zone_invites(account_id, addr, zone, message_id, observed_at)
         SELECT ?1, lower(?2), ?3, id, received_at FROM messages
         WHERE account_id = ?1 AND id = ?4",
        params![account_id, organizer, zone, message_id],
    )?;
    Ok(())
}

/// Run `work` under its own savepoint, inside the caller's transaction: the
/// ghost projection's guard (`ghosts::guarded`), for the same reasons.
///
/// An error is SWALLOWED while that transaction is alive: the savepoint rolls
/// back, the log line carries ids and an error kind only, and ingest
/// proceeds. But an error that made SQLite roll back the WHOLE transaction
/// (SQLITE_FULL, IOERR, NOMEM, a trigger's `RAISE(ROLLBACK)`) leaves the
/// connection in autocommit with the message write gone; swallowing that
/// would let whatever ingest writes next commit alone, a statement at a time.
/// So that case is returned and ingest fails as it would have without zones.
fn guarded(
    conn: &Connection,
    account: AccountId,
    message: i64,
    work: impl FnOnce() -> Result<()>,
) -> Result<()> {
    let error = match conn.execute_batch("SAVEPOINT sender_zone") {
        Err(e) => CoreError::from(e),
        Ok(()) => match work() {
            Ok(()) => match conn.execute_batch("RELEASE sender_zone") {
                Ok(()) => return Ok(()),
                Err(e) => e.into(),
            },
            Err(e) => e,
        },
    };
    let kind = super::ghosts::error_kind(&error);
    if conn.is_autocommit() {
        eprintln!(
            "squelch: timezone observation failed for account {account} message {message} ({kind}) and the transaction was rolled back; the write fails"
        );
        return Err(error);
    }
    eprintln!(
        "squelch: timezone observation failed for account {account} message {message} ({kind}); the write proceeds without it"
    );
    // Fails harmlessly when SAVEPOINT itself was what failed.
    let _ = conn.execute_batch("ROLLBACK TO sender_zone; RELEASE sender_zone");
    Ok(())
}

/// [`observe`] under [`guarded`], for the ingest transaction. Err only when
/// the transaction itself is gone; the caller propagates it.
pub(super) fn observe_guarded(
    conn: &Connection,
    account_id: AccountId,
    message_id: i64,
    invite: Option<&InviteZone>,
) -> Result<()> {
    guarded(conn, account_id, message_id, || {
        observe(conn, account_id, message_id, invite, Utc::now())
    })
}

/// [`refresh_for_message`] under [`guarded`], for a message that just became
/// countable without passing through ingest (the not-spam action).
pub(super) fn refresh_for_message_guarded(
    conn: &Connection,
    account_id: AccountId,
    message_id: i64,
) -> Result<()> {
    guarded(conn, account_id, message_id, || {
        refresh_for_message(conn, account_id, message_id, Utc::now())
    })
}

/// The zone the card shows for `addr`: the override first, else the stored
/// inference. `None` when neither exists.
pub(super) fn person_timezone_conn(
    conn: &Connection,
    account_id: AccountId,
    addr: &str,
) -> Result<Option<InferredZone>> {
    let shown = conn
        .query_row(
            "SELECT user_zone, zone, source, confidence FROM sender_zones
             WHERE account_id = ?1 AND addr = ?2",
            params![account_id, addr.trim()],
            |r| {
                let user_zone: Option<String> = r.get(0)?;
                if let Some(user) = infer_zone(user_zone.as_deref(), &[], &[], Utc::now()) {
                    return Ok(Some(user));
                }
                let (zone, source, confidence): (Option<String>, Option<String>, Option<String>) =
                    (r.get(1)?, r.get(2)?, r.get(3)?);
                Ok((|| {
                    Some(InferredZone {
                        zone: zone?,
                        source: ZoneSource::parse(source.as_deref()?)?,
                        confidence: ZoneConfidence::parse(confidence.as_deref()?)?,
                    })
                })())
            },
        )
        .optional()?;
    Ok(shown.flatten())
}

impl SqliteStore {
    /// HUMAN-DOOR ONLY: set (`Some`) or clear (`None`) the user's own word on
    /// `addr`'s zone, and return what the card now shows. Clearing hands the
    /// card back to inference, recomputed on the spot. An unknown zone name is
    /// `InvalidInput`, and so is the account's own address: the user's zone is
    /// their device's, and that address never has a row.
    pub fn set_person_timezone(
        &self,
        account_id: AccountId,
        addr: &str,
        zone: Option<&str>,
    ) -> Result<Option<InferredZone>> {
        let addr = addr.trim().to_ascii_lowercase();
        // VALIDATED, NOT CANONICALISED: the override is the user's word and is
        // stored as they wrote it. It never votes, so an alias splits nothing,
        // and renaming "Europe/Paris" to Berlin's name (one clock under the
        // curated list's rule) would show them a city they did not pick.
        let zone = match zone {
            Some(z) => Some(
                z.trim()
                    .parse::<chrono_tz::Tz>()
                    .map_err(|_| CoreError::InvalidInput("unknown timezone".into()))?
                    .name()
                    .to_string(),
            ),
            None => None,
        };
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        if is_own_address(&tx, account_id, &addr)? {
            return Err(CoreError::InvalidInput(
                "your own timezone is your device's".into(),
            ));
        }
        let now = Utc::now();
        match &zone {
            Some(zone) => {
                tx.execute(
                    "INSERT INTO sender_zones(account_id, addr, updated_at, user_zone)
                     VALUES(?1, ?2, ?3, ?4)
                     ON CONFLICT(account_id, addr) DO UPDATE SET
                         user_zone = excluded.user_zone, updated_at = excluded.updated_at",
                    params![account_id, addr, now.to_rfc3339(), zone],
                )?;
            }
            None => {
                tx.execute(
                    "UPDATE sender_zones SET user_zone = NULL WHERE account_id = ?1 AND addr = ?2",
                    params![account_id, addr],
                )?;
                refresh_sender_zone_conn(&tx, account_id, &addr, now)?;
                tx.execute(
                    "DELETE FROM sender_zones
                     WHERE account_id = ?1 AND addr = ?2 AND zone IS NULL AND user_zone IS NULL",
                    params![account_id, addr],
                )?;
            }
        }
        let shown = person_timezone_conn(&tx, account_id, &addr)?;
        tx.commit()?;
        Ok(shown)
    }
}
