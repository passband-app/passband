//! Per-device human-door tokens, and the pairing codes that issue them.
//!
//! This is the credential layer under `squelch-api`'s bearer check. Its whole
//! job is to make the answer to "may this request in" a revocable, per-device
//! fact instead of one shared secret that every client holds and nobody can
//! retire alone.
//!
//! THREE RULES SHAPE EVERY FUNCTION HERE:
//!
//! 1. **Plaintext exists once.** A token and a pairing code are generated, handed
//!    to exactly one caller, and thereafter live only as a lowercase hex SHA-256.
//!    Nothing in this module writes either to the audit log, an error string, or
//!    a `Debug` impl.
//! 2. **Verification is a point lookup, never a scan.** The presented secret is
//!    hashed and matched by that hash, so the work done is independent of how
//!    close a guess was. The [`squelch_httpauth::ct_eq`] on the digests
//!    afterwards is belt-and-braces for the same reason the relay has one.
//! 3. **Failure is one answer.** Unknown, revoked, expired, already-claimed and
//!    burned all reach the caller as the same `None`/[`CoreError::NotFound`], so
//!    the door above can answer 401 with an empty body and leak nothing about
//!    which of those it was.
//!
//! ACCOUNT SCOPING has one deliberate seam: `verify_device_token` and
//! `claim_pairing_code` take no `account_id`, because the secret IS the thing
//! that names the account. Both RESOLVE the account from the matched row and
//! scope every write they then do by it; every other method takes the account
//! explicitly, like the rest of the store.

use super::*;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::Duration;

use crate::owner_clock::{OwnerZone, OwnerZoneSource};
use sha2::{Digest, Sha256};

/// Prefix on every issued device token. It is INSIDE the hashed material, so it
/// is not decoration: a token is the whole string or it is nothing. Its job is
/// to make a leaked credential greppable and instantly recognizable in a paste.
pub const TOKEN_PREFIX: &str = "sqd_";

/// Entropy per device token. 32 bytes is 256 bits and encodes to 43 unpadded
/// base64url characters, so nothing is truncated (which would silently cost
/// entropy) and nothing is padded (which would need escaping in a header).
const TOKEN_BYTES: usize = 32;

/// Crockford base32, which is the point: `I`, `L`, `O` and `U` are absent, so a
/// code read off a screen and typed on a phone has no ambiguous character and no
/// accidental profanity. Exactly 32 symbols, so five random bits select one with
/// no rejection sampling and no bias.
const PAIRING_ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// Symbols per pairing code: 8 x 5 bits = 40 bits. Short enough to read aloud,
/// and only safe at that size because the code is single-use, expires in
/// minutes, and burns after [`MAX_FAILED_CLAIMS`] misses.
pub const PAIRING_CODE_LEN: usize = 8;

/// The contract's pairing TTL, for callers that have no reason to pick their
/// own. Minutes, not hours: the code is live in a window the user is actually
/// standing in front of both devices for.
pub const PAIRING_TTL_SECS: i64 = 10 * 60;

/// Misses that burn the live code. Against 40 bits this makes online guessing
/// hopeless without making a fat-fingered transcription unrecoverable — the user
/// simply mints another.
const MAX_FAILED_CLAIMS: i64 = 5;

/// How stale `last_used_at` must be before a verification rewrites it. Every
/// authenticated request would otherwise be a write, and a client holding an SSE
/// stream open plus polling would keep the store's one writer lock busy for a
/// column nobody reads at that resolution.
const LAST_USED_THROTTLE_SECS: i64 = 60;

/// How long a token's recorded client must have been quiet before a DIFFERENT
/// platform or version replaces it.
///
/// A changed tag is a new metric series, and the header is the token holder's
/// own text. With no bound, a holder who sends a new version on every request
/// mints a series per scrape, forever, in a Prometheus this daemon does not
/// own, and turns every GET into a store write on the way. An hour makes that
/// 24 series a day per token at worst.
///
/// The cost is to the honest case: an app that updates and relaunches shows its
/// new version up to an hour late, and until then its last-seen stamp stays
/// where the old version left it (nothing is written under the OLD tag for a
/// request that named a new one). That is a fine trade for a number an operator
/// reads in days.
///
/// "An hour" assumes the old client has gone quiet. Two apps sharing ONE token
/// at different versions (a dev build and the installed release, which read
/// the same keychain item) are not that: whichever was recorded first keeps
/// refreshing its own stamp, and the other is never recorded while it does.
const CLIENT_CHANGE_THROTTLE_SECS: i64 = 60 * 60;

/// Most (platform, version) pairs [`SqliteStore::client_versions_seen`] hands
/// back, newest first. Each one is a metric series, and a row keeps the last
/// version its device ran forever, so without a bound an account that has
/// paired and abandoned devices across fifty releases exports fifty series
/// nobody reads. Eight is two platforms with room for a rollout in flight on
/// several devices at once.
pub const CLIENT_VERSIONS_CAP: usize = 8;

/// Cap on a device name. It is a label in `token list`, not data, and an
/// unbounded one from an unauthenticated claim body is just a way to grow the DB.
const MAX_DEVICE_NAME_CHARS: usize = 100;

/// Audit actor for operator actions taken at the `squelchd` CLI.
const ACTOR_CLI: &str = "cli";
/// Audit actor for what the pairing flow does on the user's behalf.
const ACTOR_PAIRING: &str = "pairing";

/// Lowercase hex SHA-256 — the one spelling every hash column in this module
/// stores, and the only form of a token or code that touches disk.
fn hex_sha256(s: &str) -> String {
    use std::fmt::Write as _;
    Sha256::digest(s.as_bytes())
        .iter()
        .fold(String::with_capacity(64), |mut out, b| {
            // Writing to a String cannot fail.
            let _ = write!(out, "{b:02x}");
            out
        })
}

/// Audit `target` for a device-token row.
///
/// NAMESPACED rather than a bare decimal id, and that is load-bearing:
/// `list_audit` LEFT JOINs `messages ON m.id = CAST(a.target AS INTEGER)`, so
/// `42` would render this credential row in `/client/audit` wearing some
/// unrelated email's sender and subject. A non-numeric prefix CASTs to 0, which
/// matches no message and nulls the join out — the same property the `rule.*`
/// rows already lean on.
fn token_target(id: i64) -> String {
    format!("token:{id}")
}

/// Audit `target` for a pairing-code row; namespaced for the reason in
/// [`token_target`], and distinct from it because the two id spaces are
/// unrelated.
fn code_target(id: i64) -> String {
    format!("code:{id}")
}

/// One opaque failure for every way a secret can fail to be accepted. Carries no
/// detail BY CONSTRUCTION: the caller turns this into a bare 401, and a message
/// here would be the oracle the uniform answer exists to prevent.
fn no() -> CoreError {
    CoreError::NotFound
}

/// Mint a device token: `sqd_` + 256 bits of OS entropy as unpadded base64url.
/// `Err` only when the OS refuses randomness, which must fail the mint rather
/// than fall back to anything guessable.
fn mint_token_plaintext() -> Result<String> {
    let mut bytes = [0u8; TOKEN_BYTES];
    getrandom::fill(&mut bytes)
        .map_err(|_| CoreError::Other(anyhow::anyhow!("no OS entropy for a device token")))?;
    Ok(format!("{TOKEN_PREFIX}{}", URL_SAFE_NO_PAD.encode(bytes)))
}

/// Mint the normalized (undashed, uppercase) form of a pairing code.
///
/// Five bytes is exactly 40 bits, which is exactly eight 5-bit groups, so each
/// symbol is drawn uniformly from the alphabet with nothing discarded and no
/// modulo bias.
fn mint_code_plaintext() -> Result<String> {
    let mut bytes = [0u8; 5];
    getrandom::fill(&mut bytes)
        .map_err(|_| CoreError::Other(anyhow::anyhow!("no OS entropy for a pairing code")))?;
    let bits = bytes.iter().fold(0u64, |acc, b| (acc << 8) | u64::from(*b));
    Ok((0..PAIRING_CODE_LEN)
        .map(|i| {
            let shift = 5 * (PAIRING_CODE_LEN - 1 - i);
            PAIRING_ALPHABET[((bits >> shift) & 0x1f) as usize] as char
        })
        .collect())
}

/// `XXXXXXXX` -> `XXXX-XXXX`. Purely presentational: the claim normalizes the
/// dash away, so this never changes what is hashed.
fn format_pairing_code(code: &str) -> String {
    match code.len() {
        PAIRING_CODE_LEN => format!("{}-{}", &code[..4], &code[4..]),
        // A code of some other length cannot be split at 4 without panicking on
        // a char boundary; nothing mints one, and printing it whole is a strictly
        // safer failure than formatting it.
        _ => code.to_string(),
    }
}

/// The form a code is hashed in: uppercase, dashes and whitespace stripped. This
/// is what makes `xxxx-xxxx`, `XXXX XXXX` and `XXXXXXXX` the same credential, so
/// a user retyping what they see cannot fail on punctuation.
pub fn normalize_pairing_code(input: &str) -> String {
    input
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .flat_map(|c| c.to_uppercase())
        .collect()
}

/// The `device_tokens.name` every console session is claimed under, so
/// `squelchd token list` and the console's own devices table both show a browser
/// for what it is.
///
/// IT LIVES HERE, next to [`normalize_device_name`], and not in the console that
/// writes it, because it now has two readers that must agree letter for letter.
/// `squelch-api`'s console claims a pairing code under this name; the two
/// readers below EXCLUDE it, because a browser session is not a device somebody
/// paired. A second spelling anywhere would not fail loudly — it would quietly
/// count a browser as a client, which is the one thing the activation signal
/// exists to not do.
///
/// The flip side, accepted deliberately: a real device a user literally names
/// "console" is invisible to both readers. That costs an analytics row, never
/// access, and the alternative (a name the claim path reserves) would refuse a
/// legitimate pairing over a label.
pub const CONSOLE_DEVICE_NAME: &str = "console";

/// Trim and cap a device name, or `None` when nothing is left. Truncation rather
/// than refusal on the long end: the caller has already proved possession of a
/// pairing code by the time this matters, and stranding that behind a label
/// length would cost the user a whole re-mint.
fn normalize_device_name(name: &str) -> Option<String> {
    let trimmed = name.trim();
    (!trimmed.is_empty()).then(|| trimmed.chars().take(MAX_DEVICE_NAME_CHARS).collect())
}

/// One `device_tokens` row minus its hash, columns in SELECT order.
fn map_device_token(r: &rusqlite::Row<'_>) -> rusqlite::Result<DeviceToken> {
    Ok(DeviceToken {
        id: r.get(0)?,
        account_id: r.get(1)?,
        name: r.get(2)?,
        created_at: dt(r, 3)?,
        last_used_at: dt_opt(r, 4)?,
        revoked_at: dt_opt(r, 5)?,
    })
}

/// Insert a device-token row plus its audit entry inside a caller-owned
/// transaction, and return the plaintext with its new id.
///
/// Shared by the two ways a token comes into existence — the operator's CLI and
/// a pairing claim — so the hashing discipline and the audit row have exactly one
/// implementation. `actor`/`action` are the only difference between them.
fn insert_token(
    tx: &rusqlite::Transaction<'_>,
    account_id: AccountId,
    name: &str,
    actor: &str,
    action: &str,
) -> Result<IssuedDeviceToken> {
    let token = mint_token_plaintext()?;
    let now = Utc::now();
    tx.execute(
        "INSERT INTO device_tokens(account_id, token_hash, name, created_at)
         VALUES(?1,?2,?3,?4)",
        params![account_id, hex_sha256(&token), name, now.to_rfc3339()],
    )?;
    let id = tx.last_insert_rowid();
    // The audit row names the token by ID and by LABEL only. Neither the
    // plaintext nor its hash appears: the audit log is read by humans over the
    // human door, and a hash there would be an offline-guessing target for
    // anything that can read it.
    tx.execute(
        "INSERT INTO audit_log(account_id, ts, actor, action, target, detail)
         VALUES(?1,?2,?3,?4,?5,?6)",
        params![
            account_id,
            now.to_rfc3339(),
            actor,
            action,
            token_target(id),
            name,
        ],
    )?;
    Ok(IssuedDeviceToken {
        id,
        account_id,
        name: name.to_string(),
        token,
    })
}

impl SqliteStore {
    // ---- device tokens ---------------------------------------------------

    /// Issue a token for `account_id`. THE RETURNED PLAINTEXT IS THE ONLY COPY —
    /// show it once and drop it; the store keeps nothing that can reproduce it.
    ///
    /// `name` is trimmed and capped; empty is [`CoreError::InvalidInput`] rather
    /// than the module's uniform `no`, because this path is the operator's own
    /// CLI and a real message is a help there, not an oracle.
    pub fn issue_device_token(
        &self,
        account_id: AccountId,
        name: &str,
    ) -> Result<IssuedDeviceToken> {
        let name = normalize_device_name(name)
            .ok_or_else(|| CoreError::InvalidInput("a device token needs a name".to_string()))?;
        let mut conn = self.lock()?;
        // FAIL-CLOSED, the same shape as `set_sender_rule_audited`: the token and
        // its audit row share one transaction, so a credential can never exist
        // untraced.
        let tx = conn.transaction()?;
        let issued = insert_token(&tx, account_id, &name, ACTOR_CLI, "token.issue")?;
        tx.commit()?;
        Ok(issued)
    }

    /// Verify a presented bearer token. `Ok(None)` is the uniform "no" — unknown,
    /// malformed and revoked are indistinguishable from here.
    ///
    /// This runs on EVERY authenticated request and is deliberately uncached: a
    /// revocation has to bite on the very next request, and a SQLite point lookup
    /// on a UNIQUE index is cheaper than any invalidation scheme would be correct.
    ///
    /// `last_used_at` is refreshed at most once per
    /// [`LAST_USED_THROTTLE_SECS`], so a chatty client does not turn reads into a
    /// write storm.
    pub fn verify_device_token(&self, presented: &str) -> Result<Option<DeviceToken>> {
        self.verify_device_token_from(presented, None)
    }

    /// [`SqliteStore::verify_device_token`] for a request that also said WHICH
    /// APP it is, which is recorded on the token's row when the token is good.
    ///
    /// The three `client_*` columns move together. The same client again
    /// refreshes its stamp under the once-a-minute throttle `last_used_at` has;
    /// a platform or version that DIFFERS from what is stored waits out
    /// [`CLIENT_CHANGE_THROTTLE_SECS`] first. A token's first-ever tag is
    /// written at once: there is nothing for it to be replacing.
    ///
    /// `client: None` leaves the stored columns exactly as they are. That is
    /// what keeps them meaning "the app", not "the token": the iOS notification
    /// extension and a console session both present tokens and name nothing,
    /// and neither must be able to move or blank what the app last said.
    pub fn verify_device_token_from(
        &self,
        presented: &str,
        client: Option<&ClientTag>,
    ) -> Result<Option<DeviceToken>> {
        let presented = presented.trim();
        // A structural reject before hashing. This leaks only the token FORMAT,
        // which is public, and keeps unrelated traffic from touching the store.
        if !presented.starts_with(TOKEN_PREFIX) {
            return Ok(None);
        }
        let hash = hex_sha256(presented);
        let conn = self.lock()?;
        // `revoked_at IS NULL` lives in the WHERE, not in Rust: a tombstoned row
        // must be as absent to this method as a row that never existed.
        let row = conn
            .query_row(
                "SELECT id, account_id, name, created_at, last_used_at, revoked_at, token_hash,
                        client_platform, client_version, client_seen_at
                 FROM device_tokens
                 WHERE token_hash = ?1 AND revoked_at IS NULL",
                params![hash],
                |r| {
                    Ok((
                        map_device_token(r)?,
                        r.get::<_, String>(6)?,
                        r.get::<_, Option<String>>(7)?,
                        r.get::<_, Option<String>>(8)?,
                        dt_opt(r, 9)?,
                    ))
                },
            )
            .optional()?;
        let Some((mut token, stored_hash, stored_platform, stored_version, client_seen_at)) = row
        else {
            return Ok(None);
        };
        // Belt-and-braces. The lookup already matched on equality, so this can
        // only fail if SQLite's comparison and ours disagree; doing it in
        // constant time costs nothing and keeps the comparison honest if the
        // lookup ever becomes something looser.
        if !squelch_httpauth::ct_eq(stored_hash.as_bytes(), hash.as_bytes()) {
            return Ok(None);
        }

        let now = Utc::now();
        let older_than = |at: Option<DateTime<Utc>>, secs: i64| {
            at.is_none_or(|last| now.signed_duration_since(last) > Duration::seconds(secs))
        };
        let named = client.filter(|c| {
            let same = stored_platform.as_deref() == Some(c.platform.as_str())
                && stored_version.as_deref() == Some(c.version());
            let quiet_for = if same {
                LAST_USED_THROTTLE_SECS
            } else {
                CLIENT_CHANGE_THROTTLE_SECS
            };
            older_than(client_seen_at, quiet_for)
        });
        // Scoped by the account the row itself named, not just by id — the
        // standing invariant holds even where the id came from a row this
        // method just read.
        if let Some(client) = named {
            // The zone moves with the stamp, under the same throttle, so a
            // changed zone lands within a minute of the app's next request.
            // No zone on this request (an older app, or a name that did not
            // validate) keeps the one already stored.
            conn.execute(
                "UPDATE device_tokens
                 SET last_used_at = ?3, client_platform = ?4, client_version = ?5,
                     client_seen_at = ?3, client_timezone = COALESCE(?6, client_timezone)
                 WHERE id = ?1 AND account_id = ?2",
                params![
                    token.id,
                    token.account_id,
                    now.to_rfc3339(),
                    client.platform.as_str(),
                    client.version(),
                    client.zone().map(|z| z.name()),
                ],
            )?;
            token.last_used_at = Some(now);
        } else if older_than(token.last_used_at, LAST_USED_THROTTLE_SECS) {
            conn.execute(
                "UPDATE device_tokens SET last_used_at = ?3
                 WHERE id = ?1 AND account_id = ?2",
                params![token.id, token.account_id, now.to_rfc3339()],
            )?;
            token.last_used_at = Some(now);
        }
        Ok(Some(token))
    }

    /// Tombstone a token. `Ok(false)` when the id is unknown to this account or
    /// was already revoked, so revoking twice is not an error and cannot be used
    /// to probe another account's ids.
    ///
    /// Effective on the NEXT request: [`SqliteStore::verify_device_token`] reads
    /// the row every time, and nothing caches it.
    pub fn revoke_device_token(&self, account_id: AccountId, id: i64) -> Result<bool> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let now = Utc::now();
        // `revoked_at IS NULL` in the predicate is what makes the row count
        // honest: a second revoke matches nothing and audits nothing.
        let n = tx.execute(
            "UPDATE device_tokens SET revoked_at = ?3
             WHERE account_id = ?1 AND id = ?2 AND revoked_at IS NULL",
            params![account_id, id, now.to_rfc3339()],
        )?;
        if n == 0 {
            return Ok(false);
        }
        tx.execute(
            "INSERT INTO audit_log(account_id, ts, actor, action, target, detail)
             VALUES(?1,?2,?3,?4,?5,NULL)",
            params![
                account_id,
                now.to_rfc3339(),
                ACTOR_CLI,
                "token.revoke",
                token_target(id),
            ],
        )?;
        tx.commit()?;
        Ok(true)
    }

    /// Every token this account has ever been issued, oldest first, revoked ones
    /// included — a revocation the operator cannot see afterwards is not much of
    /// a control.
    ///
    /// `token_hash` is NOT in the SELECT. Nothing secret-shaped reaches a caller
    /// that only wants to print a table.
    pub fn list_device_tokens(&self, account_id: AccountId) -> Result<Vec<DeviceToken>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, account_id, name, created_at, last_used_at, revoked_at
             FROM device_tokens WHERE account_id = ?1 ORDER BY id ASC",
        )?;
        let out = stmt
            .query_map(params![account_id], map_device_token)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(out)
    }

    /// When a CLIENT device first paired with this mailbox, or `None` if none
    /// ever has.
    ///
    /// THE ACTIVATION FACT (issue #89): the hosted control plane can see who was
    /// invited and who signed up, and could not see whether anybody ever
    /// actually ran the app. This is the whole answer to that, and it is
    /// deliberately ONE TIMESTAMP: no name, no count, no device list. What
    /// leaves the pod is the smallest thing that answers the question.
    ///
    /// REVOKED ROWS COUNT. Revoking a device does not un-happen the pairing, and
    /// this is a historical fact rather than a statement about right now — a
    /// user who paired a phone in March and wiped it in April activated in
    /// March. [`SqliteStore::count_client_devices`] is the other question.
    ///
    /// Console sessions are excluded by name ([`CONSOLE_DEVICE_NAME`]): a
    /// browser signing in to the console is the hosted signup flow's own last
    /// step on some tenants, so counting it would report every signup as an
    /// activation and the signal would mean nothing.
    ///
    /// The minimum is folded in Rust over PARSED timestamps, never taken as
    /// `MIN(created_at)` in SQL. `created_at` is RFC3339 TEXT, and this module
    /// does not rest an ordering on string collation (see
    /// [`SqliteStore::claim_pairing_code`], which decides expiry the same way):
    /// a row written with a different offset or sub-second precision would sort
    /// wrong and the answer would be silently early or late.
    pub fn first_client_pairing_at(&self, account_id: AccountId) -> Result<Option<DateTime<Utc>>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT created_at FROM device_tokens
             WHERE account_id = ?1 AND name <> ?2",
        )?;
        let earliest = stmt
            .query_map(params![account_id, CONSOLE_DEVICE_NAME], |r| dt(r, 0))?
            .collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .min();
        Ok(earliest)
    }

    /// How many client devices can connect RIGHT NOW: unrevoked, and not a
    /// console session.
    ///
    /// A DIFFERENT QUESTION from [`SqliteStore::first_client_pairing_at`], on
    /// purpose. That one is a fact about the past that nothing can take back;
    /// this one is a gauge that goes down when somebody revokes a phone, and it
    /// exists for the operator's dashboard
    /// (`squelchd_devices_paired`) rather than for the activation stamp. Feeding
    /// this into the stamp would make activation something a revocation could
    /// undo.
    pub fn count_client_devices(&self, account_id: AccountId) -> Result<u64> {
        let conn = self.lock()?;
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM device_tokens
             WHERE account_id = ?1 AND name <> ?2 AND revoked_at IS NULL",
            params![account_id, CONSOLE_DEVICE_NAME],
            |r| r.get(0),
        )?;
        // COUNT(*) is never negative; the clamp is so a gauge can never be built
        // out of a wrapped cast.
        Ok(count.max(0) as u64)
    }

    /// Has ANY device token on this account ever named itself as `platform`?
    /// Revoked rows count, as in [`SqliteStore::client_versions_seen`], and
    /// unlike that listing this is not capped: fifty Mac releases cannot push
    /// the one time the phone connected out of the answer.
    pub fn client_platform_seen(
        &self,
        account_id: AccountId,
        platform: ClientPlatform,
    ) -> Result<bool> {
        let conn = self.lock()?;
        Ok(conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM device_tokens
                           WHERE account_id = ?1 AND client_platform = ?2)",
            params![account_id, platform.as_str()],
            |r| r.get(0),
        )?)
    }

    /// Which app versions have connected to this mailbox, and when each last
    /// did: one entry per (platform, version), newest first, at most
    /// [`CLIENT_VERSIONS_CAP`].
    ///
    /// For the operator's dashboard (`squelchd_client_last_seen_timestamp_seconds`),
    /// and deliberately no more than that needs: no device name, no token id,
    /// no count of devices per version. Two phones on the same build are one
    /// entry.
    ///
    /// REVOKED ROWS COUNT, for the reason they count in
    /// [`SqliteStore::first_client_pairing_at`]: "a Mac on 0.0.12 last connected
    /// on Tuesday" stays true after that Mac is unpaired. No console filter is
    /// needed here, unlike its two neighbours: a browser session never sends
    /// the header, so its row never has these columns.
    ///
    /// Folded and ordered in Rust over PARSED timestamps, like every other
    /// time comparison in this module. A stored platform this build does not
    /// know (a row written by a NEWER daemon, then a rollback) is skipped
    /// rather than exported under a label outside the closed set.
    pub fn client_versions_seen(&self, account_id: AccountId) -> Result<Vec<ClientSeen>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT client_platform, client_version, client_seen_at FROM device_tokens
             WHERE account_id = ?1 AND client_platform IS NOT NULL
               AND client_version IS NOT NULL AND client_seen_at IS NOT NULL",
        )?;
        let rows = stmt
            .query_map(params![account_id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, dt(r, 2)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        let mut newest =
            std::collections::BTreeMap::<(ClientPlatform, String), DateTime<Utc>>::new();
        for (platform, version, seen_at) in rows {
            let Some(platform) = ClientPlatform::parse(&platform) else {
                continue;
            };
            let at = newest.entry((platform, version)).or_insert(seen_at);
            *at = (*at).max(seen_at);
        }
        let mut seen = newest
            .into_iter()
            .map(|((platform, version), seen_at)| ClientSeen {
                platform,
                version,
                seen_at,
            })
            .collect::<Vec<_>>();
        // Stable over the BTreeMap's key order, so two pairs seen in the same
        // instant come out in the same order on every scrape.
        seen.sort_by_key(|c| std::cmp::Reverse(c.seen_at));
        seen.truncate(CLIENT_VERSIONS_CAP);
        Ok(seen)
    }

    /// The account owner's zone, which every agent reads "now" in (#526).
    ///
    /// The zone of the most recently USED app: each live token's activity time
    /// is its last action (`client_action_at`), or its last connection
    /// (`client_seen_at`) when it has never acted, and the newest wins. Action
    /// first because an open Mac polls: left running at home, its connection
    /// stamp keeps moving while the phone that is actually in use abroad is
    /// the one whose zone is right. A token that has never acted still counts
    /// by its connection, so a fresh install is not ignored.
    ///
    /// Revoked tokens do not count: a device that was signed out is not where
    /// the owner is. Failing every app, the primary Google Calendar's zone;
    /// failing that, UTC.
    pub fn owner_zone(&self, account_id: AccountId) -> Result<OwnerZone> {
        owner_zone_conn(&*self.lock()?, account_id)
    }

    /// Record that the app holding `presented` just DID something: stamps
    /// `client_action_at` on the token's row. The caller decides what counts
    /// (the API's bearer middleware: a successful write on the client door).
    ///
    /// Only for a row that already says it is `platform`, which is what lets
    /// [`SqliteStore::client_actions_seen`] read the stamp by the row's own
    /// platform. A token's first tag is written at once by
    /// [`SqliteStore::verify_device_token_from`], which every request passes
    /// before it can get here, so a real app's first write is not lost to this.
    ///
    /// UNTHROTTLED, unlike the stamps verification writes: those ride on
    /// reads, and this only ever follows a request that was a write already.
    /// Anything that is not a live device token (the master token, a token
    /// revoked mid-request) matches no row and is not an error.
    pub fn note_client_action(&self, presented: &str, platform: ClientPlatform) -> Result<()> {
        let presented = presented.trim();
        if !presented.starts_with(TOKEN_PREFIX) {
            return Ok(());
        }
        let conn = self.lock()?;
        conn.execute(
            "UPDATE device_tokens SET client_action_at = ?3
             WHERE token_hash = ?1 AND revoked_at IS NULL AND client_platform = ?2",
            params![
                hex_sha256(presented),
                platform.as_str(),
                Utc::now().to_rfc3339()
            ],
        )?;
        Ok(())
    }

    /// When an app of each platform last acted on this mailbox: at most one
    /// entry per platform, newest first.
    ///
    /// For the operator's dashboard
    /// (`squelchd_client_last_action_timestamp_seconds`). Revoked rows count
    /// and timestamps are compared parsed, both as in
    /// [`SqliteStore::client_versions_seen`]; so is skipping a platform this
    /// build does not know.
    pub fn client_actions_seen(&self, account_id: AccountId) -> Result<Vec<ClientActed>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT client_platform, client_action_at FROM device_tokens
             WHERE account_id = ?1 AND client_platform IS NOT NULL
               AND client_action_at IS NOT NULL",
        )?;
        let rows = stmt
            .query_map(params![account_id], |r| {
                Ok((r.get::<_, String>(0)?, dt(r, 1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        let mut newest = std::collections::BTreeMap::<ClientPlatform, DateTime<Utc>>::new();
        for (platform, acted_at) in rows {
            let Some(platform) = ClientPlatform::parse(&platform) else {
                continue;
            };
            let at = newest.entry(platform).or_insert(acted_at);
            *at = (*at).max(acted_at);
        }
        let mut acted = newest
            .into_iter()
            .map(|(platform, acted_at)| ClientActed { platform, acted_at })
            .collect::<Vec<_>>();
        acted.sort_by_key(|c| std::cmp::Reverse(c.acted_at));
        Ok(acted)
    }

    /// Whether people come back, and on which app: the retention fact the
    /// hosted control plane reads through `squelchd token activity`.
    ///
    /// The activation fact's bigger sibling, held to the same discipline:
    /// TIMESTAMPS ONLY. Per platform, when one first paired, last connected
    /// and last acted; no device name, no token id, no count. Two phones are
    /// one `ios` line carrying the earlier pairing and the later stamps.
    ///
    /// `first_paired_at` IS [`SqliteStore::first_client_pairing_at`]'s answer,
    /// called rather than re-derived, so `token first-paired` and `token
    /// activity` cannot disagree about activation.
    ///
    /// REVOKED ROWS COUNT, for "last" as well as "first". A revoked token
    /// cannot move its stamps any more, so the newest stamp it carries is a
    /// true statement about when this person last used the app, and dropping
    /// it would make "unpaired the old Mac" read as "stopped using Passband".
    /// The console is excluded by name ([`CONSOLE_DEVICE_NAME`]) exactly as
    /// the activation reader excludes it; it never sends the client header, so
    /// it would carry no platform anyway, and the name rule says so twice.
    ///
    /// A token whose app never named itself (an app older than the client
    /// header) counts toward `first_paired_at` and toward no platform. A
    /// platform this build does not know is skipped, as in
    /// [`SqliteStore::client_versions_seen`]. Folded in Rust over PARSED
    /// timestamps, like every other time comparison in this module.
    pub fn client_activity(&self, account_id: AccountId) -> Result<ClientActivity> {
        let first_paired_at = self.first_client_pairing_at(account_id)?;
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT client_platform, created_at, client_seen_at, client_action_at
             FROM device_tokens
             WHERE account_id = ?1 AND name <> ?2 AND client_platform IS NOT NULL",
        )?;
        let rows = stmt
            .query_map(params![account_id, CONSOLE_DEVICE_NAME], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    dt(r, 1)?,
                    dt_opt(r, 2)?,
                    dt_opt(r, 3)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        let mut platforms = std::collections::BTreeMap::<ClientPlatform, PlatformActivity>::new();
        for (platform, created_at, seen_at, acted_at) in rows {
            let Some(platform) = ClientPlatform::parse(&platform) else {
                continue;
            };
            let entry = platforms.entry(platform).or_insert(PlatformActivity {
                first_paired_at: created_at,
                last_seen_at: None,
                last_action_at: None,
            });
            entry.first_paired_at = entry.first_paired_at.min(created_at);
            entry.last_seen_at = entry.last_seen_at.max(seen_at);
            entry.last_action_at = entry.last_action_at.max(acted_at);
        }
        Ok(ClientActivity {
            first_paired_at,
            platforms,
        })
    }

    // ---- pairing ---------------------------------------------------------

    /// Mint a pairing code for `account_id`, SUPERSEDING every code the account
    /// already had.
    ///
    /// Superseding is a delete, not an expiry stamp: one live code at a time is
    /// the property that makes 40 bits defensible, and spent rows say nothing the
    /// audit log has not already recorded permanently.
    ///
    /// The returned code is in display form; only its normalized form is hashed.
    pub fn mint_pairing_code(
        &self,
        account_id: AccountId,
        ttl: Duration,
    ) -> Result<MintedPairingCode> {
        let code = mint_code_plaintext()?;
        let now = Utc::now();
        let expires_at = now + ttl;

        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM pairing_codes WHERE account_id = ?1",
            params![account_id],
        )?;
        tx.execute(
            "INSERT INTO pairing_codes(account_id, code_hash, expires_at, created_at)
             VALUES(?1,?2,?3,?4)",
            params![
                account_id,
                hex_sha256(&code),
                expires_at.to_rfc3339(),
                now.to_rfc3339(),
            ],
        )?;
        let id = tx.last_insert_rowid();
        // Expiry, not the code. Someone reading the audit log later needs to know
        // a pairing window was opened and when it shut, never what was in it.
        tx.execute(
            "INSERT INTO audit_log(account_id, ts, actor, action, target, detail)
             VALUES(?1,?2,?3,?4,?5,?6)",
            params![
                account_id,
                now.to_rfc3339(),
                ACTOR_CLI,
                "pairing.mint",
                code_target(id),
                format!("expires {}", expires_at.to_rfc3339()),
            ],
        )?;
        tx.commit()?;
        Ok(MintedPairingCode {
            id,
            code: format_pairing_code(&code),
            expires_at,
        })
    }

    /// Trade a pairing code for a device token. THE ONE UNAUTHENTICATED WRITE in
    /// the store: the code is the whole credential, so the account is resolved
    /// from the matched row and every write below is scoped by it.
    ///
    /// Every failure is [`CoreError::NotFound`] with nothing attached. Wrong,
    /// expired, already-claimed and burned are one answer, which is what stops a
    /// caller from mapping the code space by watching responses.
    ///
    /// A miss increments `failed_attempts` on the live code and burns it at
    /// [`MAX_FAILED_CLAIMS`]. THAT COUNTER IS PER CODE, NOT PER CALLER — the
    /// claim has no caller identity to key on — so a wrong guess spends the
    /// user's code, which is the trade that makes 40 bits safe. A malformed or
    /// wrong-length code takes the same path: brute force must never be free.
    pub fn claim_pairing_code(
        &self,
        presented_code: &str,
        device_name: &str,
    ) -> Result<IssuedDeviceToken> {
        // Checked BEFORE the code so a nameless claim cannot burn a good code.
        // It is not an oracle: the caller supplied this field and learns nothing
        // about the code from being told their own name was empty.
        let name = normalize_device_name(device_name).ok_or_else(no)?;
        let normalized = normalize_pairing_code(presented_code);
        let hash = hex_sha256(&normalized);

        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let now = Utc::now();

        // Active-ness is decided in Rust, not SQL: `expires_at` is RFC3339 text,
        // and a comparison this security-relevant should not rest on string
        // collation being an ordering on timestamps.
        let matched = tx
            .query_row(
                "SELECT id, account_id, code_hash, expires_at, claimed_at
                 FROM pairing_codes WHERE code_hash = ?1",
                params![hash],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, AccountId>(1)?,
                        r.get::<_, String>(2)?,
                        dt(r, 3)?,
                        dt_opt(r, 4)?,
                    ))
                },
            )
            .optional()?
            .filter(|(_, _, stored, expires_at, claimed_at)| {
                claimed_at.is_none()
                    && *expires_at > now
                    && squelch_httpauth::ct_eq(stored.as_bytes(), hash.as_bytes())
            });

        let Some((code_id, account_id, _, _, _)) = matched else {
            // MISS. The counter still has to land, so the transaction commits and
            // the refusal is returned after.
            Self::penalize_pairing_miss(&tx, now)?;
            tx.commit()?;
            return Err(no());
        };

        // BURN FIRST, then mint. Both are in one transaction, so a failure past
        // this point rolls the burn back with it and the user's code survives —
        // but within the transaction the code is spent before it can buy
        // anything, which is what makes the claim one-shot.
        tx.execute(
            "UPDATE pairing_codes SET claimed_at = ?3 WHERE id = ?1 AND account_id = ?2",
            params![code_id, account_id, now.to_rfc3339()],
        )?;
        let issued = insert_token(&tx, account_id, &name, ACTOR_PAIRING, "pairing.claim")?;
        tx.commit()?;
        Ok(issued)
    }

    /// Charge a failed claim against every live code and burn the ones that have
    /// run out of attempts.
    ///
    /// THE ONE CARVE-OUT FROM ACCOUNT SCOPING IN THIS MODULE, and it is
    /// deliberate. Every other statement here names an `account_id`; this SELECT
    /// cannot, because a miss matched no row and therefore named no account. A
    /// wrong guess consequently charges an attempt against EVERY live code in the
    /// store, not just the one it was aimed at.
    ///
    /// That is acceptable only under the single-tenant commitment in
    /// docs/HOSTED.md ("Per-user daemon, not a multi-tenant rewrite"): one
    /// account per daemon, and by construction at most one live code per account,
    /// so in practice this is a single row belonging to the only user there is.
    ///
    /// MUST REVISIT IF FLEET MODE EVER LANDS. The moment one store holds several
    /// accounts, this is a cross-account denial of service: anyone who can reach
    /// `/client/pair` burns every tenant's pairing window at once. There is no
    /// correct fix at this layer (the claim genuinely has no account to scope by),
    /// so the fix would have to arrive with the multi-account design itself.
    fn penalize_pairing_miss(tx: &rusqlite::Transaction<'_>, now: DateTime<Utc>) -> Result<()> {
        let live: Vec<(i64, AccountId, i64)> = {
            let mut stmt = tx.prepare(
                "SELECT id, account_id, failed_attempts, expires_at
                 FROM pairing_codes WHERE claimed_at IS NULL",
            )?;
            stmt.query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, AccountId>(1)?,
                    r.get::<_, i64>(2)?,
                    dt(r, 3)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            // An already-expired code is not live and must not be "burned":
            // burning it would append an audit row for something the clock
            // already handled.
            .filter(|(_, _, _, expires_at)| *expires_at > now)
            .map(|(id, account_id, attempts, _)| (id, account_id, attempts))
            .collect()
        };

        for (id, account_id, attempts) in live {
            if attempts + 1 < MAX_FAILED_CLAIMS {
                tx.execute(
                    "UPDATE pairing_codes SET failed_attempts = failed_attempts + 1
                     WHERE id = ?1 AND account_id = ?2",
                    params![id, account_id],
                )?;
                continue;
            }
            // Out of attempts: the row goes, so the next claim finds nothing and
            // answers exactly as it would for a wrong code.
            tx.execute(
                "DELETE FROM pairing_codes WHERE id = ?1 AND account_id = ?2",
                params![id, account_id],
            )?;
            tx.execute(
                "INSERT INTO audit_log(account_id, ts, actor, action, target, detail)
                 VALUES(?1,?2,?3,?4,?5,?6)",
                params![
                    account_id,
                    now.to_rfc3339(),
                    ACTOR_PAIRING,
                    "pairing.burn",
                    code_target(id),
                    format!("{MAX_FAILED_CLAIMS} failed claims"),
                ],
            )?;
        }
        Ok(())
    }
}

/// [`SqliteStore::owner_zone`] on a connection the caller holds.
pub(super) fn owner_zone_conn(conn: &Connection, account_id: AccountId) -> Result<OwnerZone> {
    let mut stmt = conn.prepare(
        "SELECT client_timezone, client_action_at, client_seen_at FROM device_tokens
         WHERE account_id = ?1 AND revoked_at IS NULL
           AND client_timezone IS NOT NULL AND client_seen_at IS NOT NULL",
    )?;
    let rows = stmt
        .query_map(params![account_id], |r| {
            Ok((r.get::<_, String>(0)?, dt_opt(r, 1)?, dt(r, 2)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    // Compared parsed, not as text, as in `client_versions_seen`.
    let newest = rows
        .into_iter()
        .filter_map(|(zone, acted, seen)| {
            Some((
                acted.unwrap_or(seen),
                crate::owner_clock::device_zone(&zone)?,
            ))
        })
        .max_by_key(|(at, _)| *at);
    if let Some((_, zone)) = newest {
        return Ok(OwnerZone {
            zone,
            source: OwnerZoneSource::Client,
        });
    }
    let calendar = conn
        .query_row(
            "SELECT time_zone FROM calendar_calendars
             WHERE account_id = ?1 AND is_primary = 1 AND time_zone IS NOT NULL
             LIMIT 1",
            params![account_id],
            |r| r.get::<_, String>(0),
        )
        .optional()?;
    Ok(calendar
        .as_deref()
        .and_then(crate::owner_clock::device_zone)
        .map(|zone| OwnerZone {
            zone,
            source: OwnerZoneSource::Calendar,
        })
        .unwrap_or_default())
}
