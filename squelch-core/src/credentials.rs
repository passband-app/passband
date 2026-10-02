//! Credential storage: [`OAuthToken`] in memory, [`StoredToken`] as the JSON blob
//! persisted to a backend. TWO-DOOR: each account has up to two slots keyed by
//! [`CredentialKind`] — Read (`gmail.readonly`, sync + triage) and Write
//! (`gmail.modify` + `gmail.send`, human-door actions). A store is bound to ONE
//! kind and can never return the other's token. Tokens are never logged.

use crate::config::OAuthClientConfig;
use crate::error::{CoreError, Result};
use crate::types::AccountId;
use async_trait::async_trait;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The keyring service name. One entry per (account email, kind) slot.
pub const KEYRING_SERVICE: &str = "squelch";

/// Which credential a store is bound to. Determines both the OAuth scopes that
/// minted the token and the storage slot it lives in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CredentialKind {
    /// Read-only (`gmail.readonly`). Used by sync + triage.
    Read,
    /// Read/write (`gmail.modify` + `gmail.send`). Human-door actions only.
    Write,
    /// Google Calendar (`calendar.events`, `calendar.calendarlist.readonly`),
    /// minted by `squelchd auth --calendar`. SELF-HOST ONLY: a hosted tenant's
    /// credentials file is replaced whole on every install, so a separate slot
    /// there would be wiped by the next Gmail reconnect, and hosted carries its
    /// calendar grant on the one token in the Read and Write slots instead. See
    /// [`CalendarCredentials`] for how the two shapes are read.
    Calendar,
}

impl CredentialKind {
    /// Storage-slot suffix. Read's MUST stay empty — already-issued tokens live in
    /// the plain-email slot, and suffixing it orphans them.
    pub fn slot_suffix(self) -> &'static str {
        match self {
            CredentialKind::Read => "",
            CredentialKind::Write => "#write",
            CredentialKind::Calendar => "#calendar",
        }
    }

    /// The full storage-slot key for an account email under this kind.
    pub fn slot_key(self, account_email: &str) -> String {
        format!("{account_email}{}", self.slot_suffix())
    }
}

/// An OAuth token for a Gmail account, as consumed in-memory.
#[derive(Debug, Clone)]
pub struct OAuthToken {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: Option<DateTime<Utc>>,
}

/// Abstracts where OAuth tokens live (keyring, file, env). Bound to one account and
/// one [`CredentialKind`]; `token` returns only that kind's token.
#[async_trait]
pub trait CredentialStore: Send + Sync {
    /// Return a *currently valid* access token, refreshing if necessary.
    async fn token(&self, account: AccountId) -> Result<OAuthToken>;
}

/// JSON-serialized token as persisted. Expiry is an absolute UTC instant so
/// validity survives process restarts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredToken {
    pub access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    /// Absolute expiry instant, if the provider supplied `expires_in`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    /// The scopes Google said this token carries, as of the last exchange or
    /// refresh that named them.
    ///
    /// `None` is UNKNOWN: a token stored before this field existed, or one whose
    /// every response left `scope` out. Unknown reads as "calendar not granted"
    /// and changes nothing for Gmail, whose paths have never looked at it.
    /// `#[serde(default)]` and skipped when `None`, so an old blob loads and a
    /// token with nothing recorded writes the exact bytes it always did.
    ///
    /// Only Google's own answers fill it: a consent exchange, a refresh, and the
    /// refresh an import runs. Never a value a blob or a caller supplied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scopes: Option<Vec<String>>,
}

impl StoredToken {
    /// Build from a fresh token exchange/refresh response. Scopes are left
    /// unknown; [`StoredToken::with_scopes`] records them when the response
    /// named them.
    pub fn from_response(
        access_token: String,
        refresh_token: Option<String>,
        expires_in: Option<Duration>,
    ) -> Self {
        let expires_at = expires_in
            .and_then(|d| ChronoDuration::from_std(d).ok())
            .map(|d| Utc::now() + d);
        Self {
            access_token,
            refresh_token,
            expires_at,
            scopes: None,
        }
    }

    /// The same token with its granted scopes recorded.
    pub fn with_scopes(mut self, scopes: Option<Vec<String>>) -> Self {
        self.scopes = scopes;
        self
    }

    /// Whether the recorded scopes include `scope`. Unknown is `false`.
    pub fn has_scope(&self, scope: &str) -> bool {
        self.scopes
            .as_ref()
            .is_some_and(|granted| granted.iter().any(|g| g == scope))
    }

    /// Whether this token may call the Calendar API: its recorded scopes
    /// include [`crate::config::CALENDAR_EVENTS_SCOPE`]. The list scope is not
    /// part of the answer, because without it the primary calendar is still
    /// readable.
    pub fn grants_calendar(&self) -> bool {
        self.has_scope(crate::config::CALENDAR_EVENTS_SCOPE)
    }

    /// Serialize to the JSON blob stored in a backend.
    pub fn to_json(&self) -> Result<String> {
        serde_json::to_string(self)
            .map_err(|e| CoreError::Credential(format!("serializing token: {e}")))
    }

    /// Parse from the JSON blob stored in a backend.
    pub fn from_json(s: &str) -> Result<Self> {
        serde_json::from_str(s)
            .map_err(|e| CoreError::Credential(format!("parsing stored token: {e}")))
    }

    /// True if the access token is expired or within `skew` of expiring. Tokens
    /// with no known expiry are treated as *not* expired (best effort).
    pub fn is_expired(&self, skew: ChronoDuration) -> bool {
        match self.expires_at {
            Some(exp) => Utc::now() + skew >= exp,
            None => false,
        }
    }

    fn into_oauth(self) -> OAuthToken {
        OAuthToken {
            access_token: self.access_token,
            refresh_token: self.refresh_token,
            expires_at: self.expires_at,
        }
    }
}

/// Refresh grace window: refresh when within 60s of expiry.
const REFRESH_SKEW_SECS: i64 = 60;

/// A refresh response: the fresh token, plus the scopes Google named on it.
///
/// `token.scopes` carries the same answer. The separate field stays because
/// the transfer verification has to tell "Google named these" from "Google
/// named nothing", and a token whose scopes were carried forward from storage
/// would blur the two.
pub struct RefreshedToken {
    pub token: StoredToken,
    /// `None` when the response named no scopes at all.
    pub granted_scopes: Option<Vec<String>>,
}

/// Exchange a refresh token for a fresh access token at Google's token endpoint.
/// Pure network op — persistence is the caller's job. Google usually omits a new
/// refresh token on refresh, so the caller's is carried forward when absent.
pub fn refresh_stored_token(
    client: &OAuthClientConfig,
    refresh_token: &str,
) -> Result<StoredToken> {
    Ok(refresh_stored_token_detailed(client, refresh_token)?.token)
}

/// [`refresh_stored_token`] plus the scopes Google reported. Same one round
/// trip: the two differ only in how much of the answer is kept.
pub fn refresh_stored_token_detailed(
    client: &OAuthClientConfig,
    refresh_token: &str,
) -> Result<RefreshedToken> {
    refresh_stored_token_detailed_at(
        client,
        refresh_token,
        crate::auth::GOOGLE_TOKEN_URL,
        crate::auth::EXCHANGE_HTTP_TIMEOUT,
    )
}

/// The refresh against an explicit endpoint and budget. `token_url` is a
/// parameter only so the refusals downstream of a refresh can be exercised
/// against a scripted socket; every caller in the daemon passes Google's.
///
/// The budget is NOT optional. This runs inside the sync loop's blocking
/// refresh and inside `squelchd auth --import`, so a token endpoint that
/// accepts the connection and then says nothing would wedge either of them for
/// as long as it cared to hold it.
pub(crate) fn refresh_stored_token_detailed_at(
    client: &OAuthClientConfig,
    refresh_token: &str,
    token_url: &str,
    timeout: Duration,
) -> Result<RefreshedToken> {
    use oauth2::basic::BasicClient;
    use oauth2::{AuthUrl, ClientId, ClientSecret, RefreshToken, TokenResponse, TokenUrl};

    let oauth = BasicClient::new(ClientId::new(client.client_id.clone()))
        .set_client_secret(ClientSecret::new(client.client_secret.clone()))
        .set_auth_uri(
            AuthUrl::new("https://accounts.google.com/o/oauth2/v2/auth".to_string())
                .map_err(|e| CoreError::Credential(format!("bad auth url: {e}")))?,
        )
        .set_token_uri(
            TokenUrl::new(token_url.to_string())
                .map_err(|e| CoreError::Credential(format!("bad token url: {e}")))?,
        );

    // Same two properties the guarded exchange carries, and for the same
    // reasons: redirects are REFUSED because this request holds the client
    // secret and an open redirect on the token endpoint is SSRF, and the round
    // trip is bounded by a stated budget rather than by whatever reqwest's
    // default happens to be in some future version.
    let http = oauth2::reqwest::blocking::ClientBuilder::new()
        .redirect(oauth2::reqwest::redirect::Policy::none())
        .timeout(timeout)
        .build()
        .map_err(|e| CoreError::Credential(format!("building http client: {e}")))?;

    let resp = oauth
        .exchange_refresh_token(&RefreshToken::new(refresh_token.to_string()))
        .request(&http)
        .map_err(|e| CoreError::Credential(refresh_error_message(&e.to_string())))?;

    let new_refresh = resp
        .refresh_token()
        .map(|r| r.secret().to_string())
        .or_else(|| Some(refresh_token.to_string()));
    let granted_scopes: Option<Vec<String>> = resp
        .scopes()
        .map(|granted| granted.iter().map(|s| s.to_string()).collect());

    Ok(RefreshedToken {
        token: StoredToken::from_response(
            resp.access_token().secret().to_string(),
            new_refresh,
            resp.expires_in(),
        )
        .with_scopes(granted_scopes.clone()),
        granted_scopes,
    })
}

/// The token to persist after a refresh of `stored`.
///
/// A refresh response MAY omit `scope`, and when it does the grant is the one
/// already on record, not an empty one: dropping the stored scopes there would
/// quietly un-grant calendar on the first hourly refresh. When the response
/// DOES name them it is the newer answer and wins, which is how a scope the
/// user revoked at Google stops reading as granted.
fn carry_scopes_forward(stored: &StoredToken, refreshed: RefreshedToken) -> StoredToken {
    let mut fresh = refreshed.token;
    if refreshed.granted_scopes.is_none() {
        fresh.scopes = stored.scopes.clone();
    }
    fresh
}

/// The credential-error message for a failed refresh exchange. `invalid_grant` means
/// the REFRESH token itself is dead, not a transient failure — no backoff recovers
/// it, so the message names the fix instead.
fn refresh_error_message(err: &str) -> String {
    if err.contains("invalid_grant") {
        format!(
            "refresh failed: {err} — Google has expired or revoked the refresh \
             token; re-authorize with `squelchd auth` (add --write for the write \
             credential). If this recurs every ~7 days, publish the OAuth consent \
             screen from \"Testing\" to \"In production\" in Google Cloud Console: \
             testing-status apps get 7-day refresh tokens"
        )
    } else {
        format!("refresh failed: {err}")
    }
}

/// Return a currently-valid [`OAuthToken`] from `stored`, refreshing when inside the
/// skew window. `persist` re-saves the refreshed blob into the caller's backend.
fn validate_or_refresh(
    stored: StoredToken,
    client: &OAuthClientConfig,
    persist: impl FnOnce(&StoredToken) -> Result<()>,
) -> Result<OAuthToken> {
    validate_or_refresh_at(
        stored,
        client,
        crate::auth::GOOGLE_TOKEN_URL,
        crate::auth::EXCHANGE_HTTP_TIMEOUT,
        persist,
    )
}

/// [`validate_or_refresh`] against an explicit token endpoint, so a test can
/// drive the whole refresh-and-persist path against a scripted socket.
fn validate_or_refresh_at(
    stored: StoredToken,
    client: &OAuthClientConfig,
    token_url: &str,
    timeout: Duration,
    persist: impl FnOnce(&StoredToken) -> Result<()>,
) -> Result<OAuthToken> {
    if stored.is_expired(ChronoDuration::seconds(REFRESH_SKEW_SECS)) {
        let refresh = stored.refresh_token.clone().ok_or_else(|| {
            CoreError::Credential(
                "access token expired and no refresh token is stored; re-run `squelchd auth`"
                    .to_string(),
            )
        })?;
        let refreshed = refresh_stored_token_detailed_at(client, &refresh, token_url, timeout)?;
        let fresh = carry_scopes_forward(&stored, refreshed);
        persist(&fresh)?;
        return Ok(fresh.into_oauth());
    }
    Ok(stored.into_oauth())
}

/// Persist a token into the OS keyring at `(service = "squelch", slot)` where
/// `slot` = email + kind suffix.
pub fn store_token(account_email: &str, kind: CredentialKind, token: &StoredToken) -> Result<()> {
    let slot = kind.slot_key(account_email);
    let entry = keyring::Entry::new(KEYRING_SERVICE, &slot)
        .map_err(|e| CoreError::Credential(format!("opening keyring entry: {e}")))?;
    entry
        .set_password(&token.to_json()?)
        .map_err(|e| CoreError::Credential(format!("writing keyring entry: {e}")))?;
    Ok(())
}

/// Remove an account's kind slot from the keyring. A slot that was not there is
/// not an error: the caller wants it gone, and it is.
///
/// Exists for rollback. A multi-credential import that fails partway has to put
/// the slots it already wrote back the way it found them, and "the way it found
/// them" is sometimes empty.
pub fn clear_token(account_email: &str, kind: CredentialKind) -> Result<()> {
    let slot = kind.slot_key(account_email);
    let entry = keyring::Entry::new(KEYRING_SERVICE, &slot)
        .map_err(|e| CoreError::Credential(format!("opening keyring entry: {e}")))?;
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(CoreError::Credential(format!(
            "removing keyring entry: {e}"
        ))),
    }
}

/// Read the raw stored token for an account's kind slot from the keyring.
pub fn load_token(account_email: &str, kind: CredentialKind) -> Result<StoredToken> {
    let slot = kind.slot_key(account_email);
    let entry = keyring::Entry::new(KEYRING_SERVICE, &slot)
        .map_err(|e| CoreError::Credential(format!("opening keyring entry: {e}")))?;
    let json = entry.get_password().map_err(|e| {
        CoreError::Credential(format!(
            "no stored credentials for {account_email} ({kind:?} slot) \
             (run `squelchd auth` first): {e}"
        ))
    })?;
    StoredToken::from_json(&json)
}

/// Keyring-backed credential store with transparent refresh, bound to one
/// account and one [`CredentialKind`].
pub struct KeyringCredentialStore {
    account_id: AccountId,
    account_email: String,
    kind: CredentialKind,
    client: OAuthClientConfig,
}

impl KeyringCredentialStore {
    /// Construct a Read-bound store.
    pub fn new(account_id: AccountId, account_email: String, client: OAuthClientConfig) -> Self {
        Self::new_with_kind(account_id, account_email, CredentialKind::Read, client)
    }

    /// Construct a store bound to an explicit kind.
    pub fn new_with_kind(
        account_id: AccountId,
        account_email: String,
        kind: CredentialKind,
        client: OAuthClientConfig,
    ) -> Self {
        Self {
            account_id,
            account_email,
            kind,
            client,
        }
    }

    /// The account email this store is bound to.
    pub fn account_email(&self) -> &str {
        &self.account_email
    }

    /// The credential kind this store is bound to.
    pub fn kind(&self) -> CredentialKind {
        self.kind
    }

    /// Synchronous core: load this kind's slot, refresh if needed, re-persist.
    fn valid_token_blocking(&self) -> Result<OAuthToken> {
        let stored = load_token(&self.account_email, self.kind)?;
        let email = self.account_email.clone();
        let kind = self.kind;
        validate_or_refresh(stored, &self.client, |fresh| {
            store_token(&email, kind, fresh)
        })
    }

    fn clone_for_blocking(&self) -> KeyringCredentialStore {
        KeyringCredentialStore {
            account_id: self.account_id,
            account_email: self.account_email.clone(),
            kind: self.kind,
            client: self.client.clone(),
        }
    }
}

#[async_trait]
impl CredentialStore for KeyringCredentialStore {
    async fn token(&self, account: AccountId) -> Result<OAuthToken> {
        if account != self.account_id {
            return Err(CoreError::Credential(format!(
                "account {account} not managed by this store (bound to {})",
                self.account_id
            )));
        }
        let store = self.clone_for_blocking();
        tokio::task::spawn_blocking(move || store.valid_token_blocking())
            .await
            .map_err(|e| CoreError::Credential(format!("join error: {e}")))?
    }
}

// --- encryption at rest (hosted only) --------------------------------------
//
// On a HOSTED box the credentials file is written by a control plane we run,
// not by the human who owns the mailbox, so it must never exist as plaintext on
// a disk we operate. The control plane holds only the age RECIPIENT (a public
// key) and seals the blob the instant Google's token exchange returns; the age
// IDENTITY (the private key) lives on the daemon box outside every tenant data
// dir, and systemd hands its path to each tenant daemon. Warden, Railway, and
// the JSON wire in between only ever carry ciphertext.
//
// Self-host is untouched: the env var is unset, no encryption happens, and the
// bytes on disk are exactly the bytes that shipped before this existed.

/// Names the age identity file that opens this host's credentials file — and,
/// x25519 being what it is, seals it too, since the identity carries its own
/// recipient.
///
/// UNSET (self-host, the default) means every read and write here behaves
/// exactly as it did before encryption existed. SET means every write from this
/// process is age ciphertext; a plaintext write under a configured identity is a
/// bug, never a fallback, so the failures below are all loud.
pub const CRED_AGE_IDENTITY_ENV: &str = "SQUELCH_CRED_AGE_IDENTITY";

/// First line of an ASCII-armored age file. Reads sniff for this rather than
/// trusting configuration, so a box that is handed sealed credentials with no
/// identity says so instead of failing as mangled JSON.
const AGE_ARMOR_BEGIN: &str = "-----BEGIN AGE ENCRYPTED FILE-----";

/// First bytes of a *binary* age file. We only ever write armor, but a human
/// recovering a box may well drop a plain `age -e` output here, and "that is an
/// age file you have no key for" beats "invalid JSON at line 1".
const AGE_BINARY_MAGIC: &str = "age-encryption.org/v1";

/// This host's age identity, plus the path it was read from so every failure can
/// name the file an operator has to go look at.
///
/// Deliberately NOT `Debug`: this holds a private key, and the cheapest way for
/// one to end up in a log line is a struct that will happily print itself.
struct AgeIdentity {
    identity: age::x25519::Identity,
    source: PathBuf,
}

impl AgeIdentity {
    /// Parse an `age-keygen` identity file: comment lines, blank lines, and one
    /// or more `AGE-SECRET-KEY-1…` lines, of which the first usable one wins.
    ///
    /// No line content ever reaches an error message. The whole point of the
    /// file is that its contents are the secret.
    fn read(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).map_err(|e| {
            CoreError::Credential(format!(
                "reading the age identity file {} named by {CRED_AGE_IDENTITY_ENV}: {e}",
                path.display()
            ))
        })?;

        // A line that announces itself as a secret key and then fails to parse
        // is a truncated or corrupted file — the operator needs the parser's
        // complaint, not a generic "nothing here".
        let mut malformed: Option<String> = None;
        for line in text.lines().map(str::trim) {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            match line.parse::<age::x25519::Identity>() {
                Ok(identity) => {
                    return Ok(Self {
                        identity,
                        source: path.to_path_buf(),
                    });
                }
                Err(e) if line.to_ascii_uppercase().starts_with("AGE-SECRET-KEY-1") => {
                    malformed = Some(e.to_string());
                }
                Err(_) => {}
            }
        }

        Err(CoreError::Credential(match malformed {
            Some(why) => format!(
                "the age identity file {} has a malformed secret key line: {why}",
                path.display()
            ),
            None => format!(
                "the age identity file {} holds no AGE-SECRET-KEY-1 line \
                 (generate one with `age-keygen -o <path>`)",
                path.display()
            ),
        }))
    }

    /// Seal `plaintext` to this identity's own recipient, ASCII-armored.
    fn seal(&self, plaintext: &[u8], target: &Path) -> Result<Vec<u8>> {
        age::encrypt_and_armor(&self.identity.to_public(), plaintext)
            .map(String::into_bytes)
            .map_err(|e| {
                CoreError::Credential(format!(
                    "encrypting {} to the recipient of the age identity in {}: {e}",
                    target.display(),
                    self.source.display()
                ))
            })
    }

    /// Open a sealed credentials file. Handles armored and binary age alike.
    fn open(&self, ciphertext: &[u8], target: &Path) -> Result<Vec<u8>> {
        age::decrypt(&self.identity, ciphertext).map_err(|e| {
            CoreError::Credential(format!(
                "decrypting {} with the age identity in {}: {e}",
                target.display(),
                self.source.display()
            ))
        })
    }
}

/// Resolve [`CRED_AGE_IDENTITY_ENV`] into a loaded identity, or `None` when this
/// host does not encrypt.
///
/// Unset, empty, and whitespace-only all read as "not configured" so a
/// `SQUELCH_CRED_AGE_IDENTITY=` line left in an env file cannot wedge a
/// self-host daemon. Anything else must load: a path that is missing or
/// unparseable is an error, never a quiet demotion to plaintext.
fn age_identity_from_env() -> Result<Option<AgeIdentity>> {
    let Some(raw) = std::env::var_os(CRED_AGE_IDENTITY_ENV) else {
        return Ok(None);
    };
    if raw.to_string_lossy().trim().is_empty() {
        return Ok(None);
    }
    // The path itself is NOT trimmed: trailing spaces in a filename are legal,
    // and silently opening a different file than the one configured is worse
    // than failing on a typo.
    AgeIdentity::read(Path::new(&raw)).map(Some)
}

/// True if these bytes are an age file (armored or binary) rather than JSON.
fn is_age_file(bytes: &[u8]) -> bool {
    let head = bytes.trim_ascii_start();
    head.starts_with(AGE_ARMOR_BEGIN.as_bytes()) || head.starts_with(AGE_BINARY_MAGIC.as_bytes())
}

/// On-disk shape of the credentials file: a map from slot key
/// (`email` or `email#write`) to its stored token.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct CredentialsFile {
    #[serde(default)]
    slots: BTreeMap<String, StoredToken>,
}

impl CredentialsFile {
    /// Read the file, decrypting when it is sealed.
    ///
    /// `identity` is passed in rather than resolved here so one public call
    /// resolves the env exactly once (a read-modify-write must not straddle two
    /// different answers) and so the tests can drive both shapes without
    /// mutating process env.
    ///
    /// Both directions of the migration are handled on purpose: a legacy
    /// plaintext file still loads on a box that has since been given an
    /// identity (the next write seals it), and a sealed file on a box with no
    /// identity is refused by name instead of being parsed as garbage.
    fn read(path: &Path, identity: Option<&AgeIdentity>) -> Result<Self> {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            // A missing file is an empty set of slots, not an error.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => {
                return Err(CoreError::Credential(format!(
                    "reading credentials file: {e}"
                )));
            }
        };

        let text = if is_age_file(&bytes) {
            let identity = identity.ok_or_else(|| {
                CoreError::Credential(format!(
                    "the credentials file {} is age-encrypted but {CRED_AGE_IDENTITY_ENV} \
                     is not set; point it at the age identity file that opens this box",
                    path.display()
                ))
            })?;
            let plain = identity.open(&bytes, path)?;
            String::from_utf8(plain).map_err(|e| {
                CoreError::Credential(format!(
                    "decrypted credentials file {} is not valid UTF-8: {e}",
                    path.display()
                ))
            })?
        } else {
            String::from_utf8(bytes)
                .map_err(|e| CoreError::Credential(format!("reading credentials file: {e}")))?
        };

        serde_json::from_str(&text)
            .map_err(|e| CoreError::Credential(format!("parsing credentials file: {e}")))
    }

    /// Write atomically-ish with mode 0600 on unix (temp file + rename so a
    /// crash mid-write can't truncate the existing creds), sealing first when an
    /// identity is configured.
    ///
    /// The seal happens BEFORE the bytes reach a file descriptor, so the temp
    /// file is ciphertext too — there is no window in which plaintext exists on
    /// a sealed host's disk.
    fn write(&self, path: &Path, identity: Option<&AgeIdentity>) -> Result<()> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)
                .map_err(|e| CoreError::Credential(format!("creating credentials dir: {e}")))?;
        }
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| CoreError::Credential(format!("serializing credentials: {e}")))?;
        let bytes = match identity {
            Some(identity) => identity.seal(json.as_bytes(), path)?,
            None => json.into_bytes(),
        };

        // A temp name of this write's OWN. A fixed name let two writers open
        // one temp file with truncate and interleave their bytes, and the
        // torn blob was then renamed over the real file. The process lock in
        // [`update_credentials_file`] serializes this process's writers; the
        // unique name also keeps another process (`squelchd auth` beside a
        // running daemon) from ever sharing a temp file with this one.
        let tmp = unique_temp_path(path);
        if let Err(e) = write_private(&tmp, &bytes) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
        if let Err(e) = std::fs::rename(&tmp, path) {
            let _ = std::fs::remove_file(&tmp);
            return Err(CoreError::Credential(format!(
                "finalizing credentials file: {e}"
            )));
        }
        // Be explicit about 0600: the destination may have pre-existed looser.
        set_private_mode(path)?;
        Ok(())
    }
}

/// `credentials.json.tmp.<pid>.<n>`, beside the real file so the rename stays
/// on one filesystem and atomic.
fn unique_temp_path(path: &Path) -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".tmp.{}.{n}", std::process::id()));
    path.with_file_name(name)
}

/// THE ONE READ-MODIFY-WRITE of the credentials file, under a process-wide
/// lock held across the read, the change, the temp write and the rename.
///
/// Hosted keeps one token in the Read and Write slots and the calendar
/// resolver reads the same Read slot, so three refreshers come due at the
/// same moment and all write this one file. Without the lock two of them
/// read the same old file and the second rename drops the first's update
/// (a refreshed slot reverts), and with the old fixed temp name their bytes
/// could interleave into a file that no longer decodes, which is Gmail down
/// until the credential is reinstalled. The lock covers local file work only:
/// no caller holds it across a network call (a refresh happens before
/// `persist` is called).
fn update_credentials_file(
    path: &Path,
    identity: Option<&AgeIdentity>,
    change: impl FnOnce(&mut CredentialsFile),
) -> Result<()> {
    static FILE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    // A writer that panicked mid-update left at worst a stray temp file: the
    // real file is only ever replaced by a rename. So poison is not a reason
    // to stop persisting tokens.
    let _held = FILE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut file = CredentialsFile::read(path, identity)?;
    #[cfg(test)]
    test_pause(path);
    change(&mut file);
    file.write(path, identity)
}

/// TEST HOOK: widen the read-to-rename window for a file whose name asks for
/// it, so the concurrency test does not depend on a lucky schedule.
#[cfg(test)]
fn test_pause(path: &Path) {
    if path.to_string_lossy().contains(tests::RACE_PAUSE_MARKER) {
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Write bytes to `path` creating it 0600 on unix.
///
/// Public because it is the ONE writer for any file that carries credential
/// material: the credentials file here, and the blob `squelchd auth --export
/// --out` writes. A plain redirect would take the ambient umask, which is 0644
/// on most hosts.
pub fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts
        .open(path)
        .map_err(|e| CoreError::Credential(format!("opening credentials file: {e}")))?;
    #[cfg(test)]
    test_pause(path);
    f.write_all(bytes)
        .map_err(|e| CoreError::Credential(format!("writing credentials file: {e}")))?;
    f.flush()
        .map_err(|e| CoreError::Credential(format!("flushing credentials file: {e}")))?;
    set_private_mode(path)?;
    Ok(())
}

/// Force mode 0600 on unix; no-op elsewhere.
fn set_private_mode(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| CoreError::Credential(format!("setting credentials file mode: {e}")))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// Persist a token into the JSON credentials file at `path`, in the slot for
/// `(email, kind)`. Merges with any existing slots. File ends up mode 0600.
pub fn store_token_file(
    path: &Path,
    account_email: &str,
    kind: CredentialKind,
    token: &StoredToken,
) -> Result<()> {
    store_token_file_with(
        path,
        account_email,
        kind,
        token,
        age_identity_from_env()?.as_ref(),
    )
}

/// [`store_token_file`] against an explicit identity. The env is resolved ONCE
/// by the public wrapper and threaded through both halves of the
/// read-modify-write, so a merge can never read plaintext and write ciphertext
/// under a different key (or the reverse).
fn store_token_file_with(
    path: &Path,
    account_email: &str,
    kind: CredentialKind,
    token: &StoredToken,
    identity: Option<&AgeIdentity>,
) -> Result<()> {
    update_credentials_file(path, identity, |file| {
        file.slots
            .insert(kind.slot_key(account_email), token.clone());
    })
}

/// Read the raw stored token for an account's kind slot from the file backend.
pub fn load_token_file(
    path: &Path,
    account_email: &str,
    kind: CredentialKind,
) -> Result<StoredToken> {
    load_token_file_with(path, account_email, kind, age_identity_from_env()?.as_ref())
}

/// [`load_token_file`] against an explicit identity.
fn load_token_file_with(
    path: &Path,
    account_email: &str,
    kind: CredentialKind,
    identity: Option<&AgeIdentity>,
) -> Result<StoredToken> {
    let file = CredentialsFile::read(path, identity)?;
    file.slots
        .get(&kind.slot_key(account_email))
        .cloned()
        .ok_or_else(|| {
            CoreError::Credential(format!(
                "no stored credentials for {account_email} ({kind:?} slot) in {} \
                 (run `squelchd auth` first)",
                path.display()
            ))
        })
}

// --- the sealing half, for whoever provisions a hosted tenant ---------------

/// Render the exact bytes a credentials file holds for `entries`, as the
/// plaintext a provisioner seals with [`seal_credentials_for_recipient`].
///
/// This exists so nothing outside this module has to re-derive the on-disk
/// shape. That shape is load-bearing and easy to get subtly wrong: the slot map
/// is keyed by [`CredentialKind::slot_key`], and a blob that is merely a bare
/// [`StoredToken`] JSON deserializes into an EMPTY slot map without complaint
/// (serde ignores unknown fields), so the mistake surfaces as "no stored
/// credentials" on a box you cannot easily debug rather than as a parse error.
pub fn credentials_file_plaintext(
    account_email: &str,
    entries: &[(CredentialKind, StoredToken)],
) -> Result<String> {
    let mut file = CredentialsFile::default();
    for (kind, token) in entries {
        file.slots
            .insert(kind.slot_key(account_email), token.clone());
    }
    serde_json::to_string_pretty(&file)
        .map_err(|e| CoreError::Credential(format!("serializing credentials: {e}")))
}

/// Seal bytes to an age recipient (`age1…`), ASCII-armored.
///
/// The provisioning direction: a control plane holds ONLY the recipient, so it
/// can seal a tenant's credentials and can never open one. The armor is what
/// makes the result safe to carry as a JSON string on the wire to the box.
///
/// The recipient is parsed strictly — a typo must fail here, in the exchange
/// path where the plaintext is still in hand and can be dropped, rather than
/// producing something the daemon cannot open days later.
pub fn seal_credentials_for_recipient(recipient: &str, plaintext: &[u8]) -> Result<String> {
    let recipient: age::x25519::Recipient = recipient.trim().parse().map_err(|e| {
        CoreError::Credential(format!("age recipient is not a valid age1 public key: {e}"))
    })?;
    age::encrypt_and_armor(&recipient, plaintext)
        .map_err(|e| CoreError::Credential(format!("encrypting credentials to recipient: {e}")))
}

/// File-backed credential store with transparent refresh, bound to one account
/// and one [`CredentialKind`]. For headless hosts with no Secret Service.
pub struct FileCredentialStore {
    account_id: AccountId,
    account_email: String,
    kind: CredentialKind,
    path: PathBuf,
    client: OAuthClientConfig,
}

impl FileCredentialStore {
    /// Construct a Read-bound file store.
    pub fn new(
        account_id: AccountId,
        account_email: String,
        path: PathBuf,
        client: OAuthClientConfig,
    ) -> Self {
        Self::new_with_kind(
            account_id,
            account_email,
            CredentialKind::Read,
            path,
            client,
        )
    }

    /// Construct a file store bound to an explicit kind.
    pub fn new_with_kind(
        account_id: AccountId,
        account_email: String,
        kind: CredentialKind,
        path: PathBuf,
        client: OAuthClientConfig,
    ) -> Self {
        Self {
            account_id,
            account_email,
            kind,
            path,
            client,
        }
    }

    /// The account email this store is bound to.
    pub fn account_email(&self) -> &str {
        &self.account_email
    }

    /// The credential kind this store is bound to.
    pub fn kind(&self) -> CredentialKind {
        self.kind
    }

    fn valid_token_blocking(&self) -> Result<OAuthToken> {
        let stored = load_token_file(&self.path, &self.account_email, self.kind)?;
        let path = self.path.clone();
        let email = self.account_email.clone();
        let kind = self.kind;
        validate_or_refresh(stored, &self.client, |fresh| {
            store_token_file(&path, &email, kind, fresh)
        })
    }

    fn clone_for_blocking(&self) -> FileCredentialStore {
        FileCredentialStore {
            account_id: self.account_id,
            account_email: self.account_email.clone(),
            kind: self.kind,
            path: self.path.clone(),
            client: self.client.clone(),
        }
    }
}

#[async_trait]
impl CredentialStore for FileCredentialStore {
    async fn token(&self, account: AccountId) -> Result<OAuthToken> {
        if account != self.account_id {
            return Err(CoreError::Credential(format!(
                "account {account} not managed by this store (bound to {})",
                self.account_id
            )));
        }
        let store = self.clone_for_blocking();
        tokio::task::spawn_blocking(move || store.valid_token_blocking())
            .await
            .map_err(|e| CoreError::Credential(format!("join error: {e}")))?
    }
}

use crate::config::CredentialBackend;

/// Persist a freshly-minted token into whichever backend is configured.
pub fn store_token_backend(
    backend: CredentialBackend,
    credentials_path: &Path,
    account_email: &str,
    kind: CredentialKind,
    token: &StoredToken,
) -> Result<()> {
    match backend {
        CredentialBackend::Keyring => store_token(account_email, kind, token),
        CredentialBackend::File => store_token_file(credentials_path, account_email, kind, token),
    }
}

/// Persist several tokens as ONE unit: either every slot ends up holding its new
/// token, or every slot is left the way it was found.
///
/// An import carries up to two credentials that were consented together, and a
/// backend failure on the second one must not leave the Read slot from this
/// paste next to the Write slot from a different one: the two-door split assumes
/// both slots describe the same grant.
///
/// The file backend gets that for free, since both slots live in one file and
/// one atomic write. The keyring has a slot per entry, so the prior contents are
/// captured first and put back on failure.
pub fn store_tokens_backend(
    backend: CredentialBackend,
    credentials_path: &Path,
    account_email: &str,
    entries: &[(CredentialKind, StoredToken)],
) -> Result<()> {
    match backend {
        CredentialBackend::File => {
            let identity = age_identity_from_env()?;
            update_credentials_file(credentials_path, identity.as_ref(), |file| {
                for (kind, token) in entries {
                    file.slots
                        .insert(kind.slot_key(account_email), token.clone());
                }
            })
        }
        CredentialBackend::Keyring => {
            // What each slot held BEFORE this run, in the order they were
            // written, so a failure can walk it back.
            let mut written: Vec<(CredentialKind, Option<StoredToken>)> = Vec::new();
            for (kind, token) in entries {
                // An unreadable prior entry reads as absent, and rollback then
                // clears the slot: either way nothing from this paste survives.
                let prior = load_token(account_email, *kind).ok();
                match store_token(account_email, *kind, token) {
                    Ok(()) => written.push((*kind, prior)),
                    Err(e) => {
                        let undone = rollback_keyring(account_email, &written);
                        return Err(match undone {
                            Ok(()) => e,
                            Err(names) => CoreError::Credential(format!(
                                "{e}. The {names} credential(s) written earlier in this import \
                                 could not be rolled back either, so those slots now hold the \
                                 imported token while this one does not; re-run the import."
                            )),
                        });
                    }
                }
            }
            Ok(())
        }
    }
}

/// Put keyring slots back the way they were found, newest first. `Err` carries
/// the slots that could NOT be restored, because the caller has to name them.
fn rollback_keyring(
    account_email: &str,
    written: &[(CredentialKind, Option<StoredToken>)],
) -> std::result::Result<(), String> {
    let mut stuck: Vec<String> = Vec::new();
    for (kind, prior) in written.iter().rev() {
        let undo = match prior {
            Some(token) => store_token(account_email, *kind, token),
            None => clear_token(account_email, *kind),
        };
        if undo.is_err() {
            stuck.push(format!("{kind:?}"));
        }
    }
    if stuck.is_empty() {
        Ok(())
    } else {
        Err(stuck.join(", "))
    }
}

/// Load a raw stored token from whichever backend is configured.
pub fn load_token_backend(
    backend: CredentialBackend,
    credentials_path: &Path,
    account_email: &str,
    kind: CredentialKind,
) -> Result<StoredToken> {
    match backend {
        CredentialBackend::Keyring => load_token(account_email, kind),
        CredentialBackend::File => load_token_file(credentials_path, account_email, kind),
    }
}

/// Build a Read-bound [`CredentialStore`] trait object for the configured backend.
pub fn read_store_for_backend(
    backend: CredentialBackend,
    account_id: AccountId,
    account_email: String,
    credentials_path: PathBuf,
    client: OAuthClientConfig,
) -> std::sync::Arc<dyn CredentialStore> {
    match backend {
        CredentialBackend::Keyring => std::sync::Arc::new(KeyringCredentialStore::new(
            account_id,
            account_email,
            client,
        )),
        CredentialBackend::File => std::sync::Arc::new(FileCredentialStore::new(
            account_id,
            account_email,
            credentials_path,
            client,
        )),
    }
}

// --- which token may call the Calendar API ----------------------------------

/// Pick the slot that answers for the Calendar API, given what each slot holds.
///
/// THE ORDER, and the one place it is decided:
/// 1. The Calendar slot, when its recorded scopes grant calendar. Self-host's
///    `squelchd auth --calendar` lands here, and it is the narrower grant.
/// 2. The Read slot, when ITS recorded scopes grant calendar. This is how a
///    hosted tenant is read: the calendar consent is the same token with more
///    scopes, sealed into the Read and Write slots like every hosted install.
/// 3. Nothing: calendar is not granted.
///
/// A slot whose scopes are unknown (`None`, a token stored before scopes were
/// recorded) never answers, and neither does a Calendar slot whose latest
/// refresh no longer names the events scope: that is a grant the user took
/// back at Google. The Write slot is never consulted. Pure, so the order is
/// tested without a backend.
pub fn pick_calendar_slot(
    calendar: Option<StoredToken>,
    read: Option<StoredToken>,
) -> Option<(CredentialKind, StoredToken)> {
    if let Some(token) = calendar.filter(StoredToken::grants_calendar) {
        return Some((CredentialKind::Calendar, token));
    }
    read.filter(StoredToken::grants_calendar)
        .map(|token| (CredentialKind::Read, token))
}

/// THE CALENDAR TOKEN RESOLVER: the one answer to "which token may call the
/// Calendar API", bound to one account and one configured backend.
///
/// Resolved on EVERY call rather than once at startup, because the answer
/// changes under a running daemon: a self-host runs `squelchd auth --calendar`
/// beside it, and a refresh can report the grant gone. The token it hands back
/// refreshes exactly as the other slots do, and is persisted into the slot it
/// came from, never into another.
///
/// A slot that cannot be read (absent, or a backend error) counts as holding
/// nothing, so a broken calendar answer is "not granted" and never an error on
/// a Gmail path (invariant: calendar failures do not fail Gmail).
#[derive(Clone)]
pub struct CalendarCredentials {
    account_id: AccountId,
    account_email: String,
    backend: CredentialBackend,
    credentials_path: PathBuf,
    client: OAuthClientConfig,
    /// The last answer to "is calendar granted". Shared by every clone, so
    /// the many `/client/stats` polls (every 10 seconds per client) read the
    /// backend at most once per [`GRANTED_TTL`]: on hosted that read is a file
    /// read plus an age decrypt, on a macOS self-host two Keychain reads.
    granted_cache: std::sync::Arc<std::sync::Mutex<Option<GrantedAnswer>>>,
}

/// One cached answer: when it was read, and for the file backend the file's
/// modification time then, so any write to the file (a credential install,
/// `squelchd auth --calendar`, a refresh) is seen on the next call rather
/// than after the TTL. A `stat` is the whole cost of a hit.
#[derive(Debug, Clone, Copy)]
struct GrantedAnswer {
    at: std::time::Instant,
    modified: Option<std::time::SystemTime>,
    granted: bool,
}

/// How long [`CalendarCredentials::granted`] trusts its last answer. For the
/// file backend a changed file ends it early; for the keyring, a fresh grant
/// shows up within one or two polls. The sync loop does not use the cache (it
/// resolves the token on every pass) and refreshes it with what it found.
pub const GRANTED_TTL: Duration = Duration::from_secs(15);

/// A token that may call the Calendar API, and whether it may also read the
/// user's calendar list (without that, only the primary calendar is read).
#[derive(Debug, Clone)]
pub struct CalendarAccess {
    pub token: OAuthToken,
    pub list_granted: bool,
}

impl CalendarCredentials {
    pub fn new(
        account_id: AccountId,
        account_email: String,
        backend: CredentialBackend,
        credentials_path: PathBuf,
        client: OAuthClientConfig,
    ) -> Self {
        Self {
            account_id,
            account_email,
            backend,
            credentials_path,
            client,
            granted_cache: Default::default(),
        }
    }

    /// The credentials file's modification time, for the file backend.
    fn file_modified(&self) -> Option<std::time::SystemTime> {
        match self.backend {
            CredentialBackend::File => std::fs::metadata(&self.credentials_path)
                .and_then(|m| m.modified())
                .ok(),
            CredentialBackend::Keyring => None,
        }
    }

    fn remember_granted(&self, granted: bool, modified: Option<std::time::SystemTime>) {
        if let Ok(mut cache) = self.granted_cache.lock() {
            *cache = Some(GrantedAnswer {
                at: std::time::Instant::now(),
                modified,
                granted,
            });
        }
    }

    /// Which slot answers right now, and its token as stored (not refreshed).
    /// BLOCKING: it reads the backend.
    pub fn resolve_blocking(&self) -> Option<(CredentialKind, StoredToken)> {
        let load = |kind| {
            load_token_backend(
                self.backend,
                &self.credentials_path,
                &self.account_email,
                kind,
            )
            .ok()
        };
        pick_calendar_slot(load(CredentialKind::Calendar), load(CredentialKind::Read))
    }

    /// Whether calendar is granted, without refreshing or calling Google.
    ///
    /// Answered from a cache for up to [`GRANTED_TTL`]; see `granted_cache`.
    pub async fn granted(&self) -> bool {
        let this = self.clone();
        let cache = self.granted_cache.clone();
        tokio::task::spawn_blocking(move || {
            // Stat BEFORE reading, so a write that lands between the two
            // leaves a stale mtime in the cache and is read again next time.
            let modified = this.file_modified();
            let cached = cache
                .lock()
                .ok()
                .and_then(|c| *c)
                .filter(|c| c.at.elapsed() < GRANTED_TTL && c.modified == modified);
            if let Some(answer) = cached {
                return answer.granted;
            }
            let granted = this.resolve_blocking().is_some();
            this.remember_granted(granted, modified);
            granted
        })
        .await
        .unwrap_or(false)
    }

    /// [`Self::calendar_token`] with whether the list scope is granted too,
    /// for the sync loop. Never cached; refreshes the `granted` cache.
    pub async fn calendar_access(&self, account: AccountId) -> Result<Option<CalendarAccess>> {
        if account != self.account_id {
            return Err(CoreError::Credential(format!(
                "account {account} not managed by this store (bound to {})",
                self.account_id
            )));
        }
        let this = self.clone();
        tokio::task::spawn_blocking(move || {
            let modified = this.file_modified();
            let access = this.calendar_access_blocking();
            if let Ok(found) = &access {
                this.remember_granted(found.is_some(), modified);
            }
            access
        })
        .await
        .map_err(|e| CoreError::Credential(format!("join error: {e}")))?
    }

    fn calendar_access_blocking(&self) -> Result<Option<CalendarAccess>> {
        let Some((kind, stored)) = self.resolve_blocking() else {
            return Ok(None);
        };
        let list_granted = stored.has_scope(crate::config::CALENDAR_LIST_SCOPE);
        self.refresh_blocking(kind, stored).map(|token| {
            Some(CalendarAccess {
                token,
                list_granted,
            })
        })
    }

    /// A currently valid access token for the Calendar API, or `Ok(None)` when
    /// calendar is not granted. Refreshes when inside the skew window and
    /// persists the refreshed token into the slot it was read from.
    pub async fn calendar_token(&self, account: AccountId) -> Result<Option<OAuthToken>> {
        if account != self.account_id {
            return Err(CoreError::Credential(format!(
                "account {account} not managed by this store (bound to {})",
                self.account_id
            )));
        }
        let this = self.clone();
        tokio::task::spawn_blocking(move || this.calendar_token_blocking())
            .await
            .map_err(|e| CoreError::Credential(format!("join error: {e}")))?
    }

    fn calendar_token_blocking(&self) -> Result<Option<OAuthToken>> {
        let Some((kind, stored)) = self.resolve_blocking() else {
            return Ok(None);
        };
        self.refresh_blocking(kind, stored).map(Some)
    }

    /// The slot's token, refreshed when due and persisted into that same slot
    /// through the same path the Gmail stores use, under the file backend's
    /// write lock.
    fn refresh_blocking(&self, kind: CredentialKind, stored: StoredToken) -> Result<OAuthToken> {
        let (backend, path, email) = (
            self.backend,
            self.credentials_path.clone(),
            self.account_email.clone(),
        );
        validate_or_refresh(stored, &self.client, |fresh| {
            store_token_backend(backend, &path, &email, kind, fresh)
        })
    }
}

/// The [`CredentialStore`] shape of the same resolver, for code that already
/// takes one. "Not granted" is an error here because the trait has no other
/// way to say it; [`CalendarCredentials::calendar_token`] is the call that
/// tells the two apart.
#[async_trait]
impl CredentialStore for CalendarCredentials {
    async fn token(&self, account: AccountId) -> Result<OAuthToken> {
        self.calendar_token(account).await?.ok_or_else(|| {
            CoreError::Credential("Google Calendar is not granted for this mailbox".to_string())
        })
    }
}

/// Env-var backed stub for the v0 skeleton. Still handy for tests / CI without a
/// keyring. Real deployments use [`KeyringCredentialStore`] / [`FileCredentialStore`].
pub struct EnvCredentialStore;

#[async_trait]
impl CredentialStore for EnvCredentialStore {
    async fn token(&self, _account: AccountId) -> Result<OAuthToken> {
        let access_token = std::env::var("SQUELCH_ACCESS_TOKEN").unwrap_or_default();
        Ok(OAuthToken {
            access_token,
            refresh_token: None,
            expires_at: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client() -> OAuthClientConfig {
        OAuthClientConfig {
            client_id: "id".into(),
            client_secret: "secret".into(),
        }
    }

    #[test]
    fn invalid_grant_refresh_error_names_the_fix() {
        let msg = refresh_error_message(
            "Server returned error response: invalid_grant: Token has been expired or revoked.",
        );
        assert!(msg.contains("squelchd auth"));
        assert!(msg.contains("In production"));

        let transient = refresh_error_message("connection reset by peer");
        assert_eq!(transient, "refresh failed: connection reset by peer");
    }

    #[test]
    fn token_json_round_trip() {
        let t = StoredToken {
            access_token: "aaa".to_string(),
            refresh_token: Some("rrr".to_string()),
            expires_at: Some(Utc::now()),
            scopes: None,
        };
        let json = t.to_json().unwrap();
        let back = StoredToken::from_json(&json).unwrap();
        assert_eq!(t, back);
    }

    #[test]
    fn token_json_round_trip_no_optionals() {
        let t = StoredToken {
            access_token: "aaa".to_string(),
            refresh_token: None,
            expires_at: None,
            scopes: None,
        };
        let json = t.to_json().unwrap();
        assert!(!json.contains("refresh_token"));
        assert!(!json.contains("expires_at"));
        let back = StoredToken::from_json(&json).unwrap();
        assert_eq!(t, back);
    }

    #[test]
    fn expiry_logic() {
        let skew = ChronoDuration::seconds(60);

        let past = StoredToken {
            access_token: "a".into(),
            refresh_token: None,
            expires_at: Some(Utc::now() - ChronoDuration::seconds(10)),
            scopes: None,
        };
        assert!(past.is_expired(skew));

        let soon = StoredToken {
            access_token: "a".into(),
            refresh_token: None,
            expires_at: Some(Utc::now() + ChronoDuration::seconds(30)),
            scopes: None,
        };
        assert!(soon.is_expired(skew));

        let future = StoredToken {
            access_token: "a".into(),
            refresh_token: None,
            expires_at: Some(Utc::now() + ChronoDuration::hours(1)),
            scopes: None,
        };
        assert!(!future.is_expired(skew));

        let unknown = StoredToken {
            access_token: "a".into(),
            refresh_token: None,
            expires_at: None,
            scopes: None,
        };
        assert!(!unknown.is_expired(skew));
    }

    #[test]
    fn from_response_sets_absolute_expiry() {
        let t = StoredToken::from_response(
            "tok".into(),
            Some("ref".into()),
            Some(Duration::from_secs(3600)),
        );
        let exp = t.expires_at.expect("expiry set");
        let delta = (exp - Utc::now()).num_seconds();
        assert!((3500..=3600).contains(&delta), "delta was {delta}");
    }

    #[test]
    fn slot_keys_differ_by_kind() {
        assert_eq!(CredentialKind::Read.slot_key("you@x.com"), "you@x.com");
        assert_eq!(
            CredentialKind::Write.slot_key("you@x.com"),
            "you@x.com#write"
        );
        assert_ne!(
            CredentialKind::Read.slot_key("you@x.com"),
            CredentialKind::Write.slot_key("you@x.com")
        );
    }

    fn tmp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "squelch-cred-{}-{}-{}.json",
            std::process::id(),
            name,
            Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ))
    }

    #[test]
    fn file_store_round_trip() {
        let path = tmp_path("roundtrip");
        let tok = StoredToken {
            access_token: "read-access".into(),
            refresh_token: Some("read-refresh".into()),
            expires_at: Some(Utc::now() + ChronoDuration::hours(1)),
            scopes: None,
        };
        store_token_file(&path, "you@x.com", CredentialKind::Read, &tok).unwrap();
        let back = load_token_file(&path, "you@x.com", CredentialKind::Read).unwrap();
        assert_eq!(tok, back);
        std::fs::remove_file(&path).ok();
    }

    #[cfg(unix)]
    #[test]
    fn file_store_is_0600() {
        use std::os::unix::fs::PermissionsExt;
        let path = tmp_path("mode");
        let tok = StoredToken {
            access_token: "a".into(),
            refresh_token: None,
            expires_at: None,
            scopes: None,
        };
        store_token_file(&path, "you@x.com", CredentialKind::Read, &tok).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "mode was {:o}", mode & 0o777);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn file_store_kind_slot_separation() {
        // A write token stored under Write must NEVER come back for a Read load.
        let path = tmp_path("kindsep");
        let read_tok = StoredToken {
            access_token: "READ-ONLY-TOKEN".into(),
            refresh_token: None,
            expires_at: None,
            scopes: None,
        };
        let write_tok = StoredToken {
            access_token: "WRITE-CAPABLE-TOKEN".into(),
            refresh_token: None,
            expires_at: None,
            scopes: None,
        };
        store_token_file(&path, "you@x.com", CredentialKind::Read, &read_tok).unwrap();
        store_token_file(&path, "you@x.com", CredentialKind::Write, &write_tok).unwrap();

        let got_read = load_token_file(&path, "you@x.com", CredentialKind::Read).unwrap();
        let got_write = load_token_file(&path, "you@x.com", CredentialKind::Write).unwrap();
        assert_eq!(got_read.access_token, "READ-ONLY-TOKEN");
        assert_eq!(got_write.access_token, "WRITE-CAPABLE-TOKEN");
        assert_ne!(got_read.access_token, got_write.access_token);
        std::fs::remove_file(&path).ok();
    }

    /// An import carries credentials that were consented together, so the two
    /// slots must never end up describing different consents.
    #[test]
    fn a_multi_credential_store_lands_whole_or_not_at_all() {
        // In a directory of its own, so the failure below can be staged by
        // making that directory refuse new files.
        let dir = tmp_path("allornothing").with_extension("d");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("credentials.json");
        let tok = |access: &str| StoredToken {
            access_token: access.into(),
            refresh_token: Some("r".into()),
            expires_at: None,
            scopes: None,
        };

        // A slot from an earlier import, to prove a failed one leaves it alone.
        store_token_file(&path, "you@x.com", CredentialKind::Read, &tok("old-read")).unwrap();

        // The file backend writes once, so both slots arrive together.
        store_tokens_backend(
            CredentialBackend::File,
            &path,
            "you@x.com",
            &[
                (CredentialKind::Read, tok("new-read")),
                (CredentialKind::Write, tok("new-write")),
            ],
        )
        .unwrap();
        assert_eq!(
            load_token_file(&path, "you@x.com", CredentialKind::Read)
                .unwrap()
                .access_token,
            "new-read"
        );
        assert_eq!(
            load_token_file(&path, "you@x.com", CredentialKind::Write)
                .unwrap()
                .access_token,
            "new-write"
        );

        // A directory that refuses the temp file fails the write with both
        // slots still in hand, which is the case that must change nothing.
        // (Every write has a temp name of its own now, so the old trick of a
        // directory squatting on a fixed temp name no longer applies.)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
        }
        let err = store_tokens_backend(
            CredentialBackend::File,
            &path,
            "you@x.com",
            &[
                (CredentialKind::Read, tok("doomed-read")),
                (CredentialKind::Write, tok("doomed-write")),
            ],
        );
        assert!(err.is_err(), "the write should have failed");
        assert_eq!(
            load_token_file(&path, "you@x.com", CredentialKind::Read)
                .unwrap()
                .access_token,
            "new-read",
            "a failed store must not land any of its entries"
        );
        assert_eq!(
            load_token_file(&path, "you@x.com", CredentialKind::Write)
                .unwrap()
                .access_token,
            "new-write"
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn file_store_read_bound_never_returns_write() {
        // A Read-bound FileCredentialStore must fail (NotFound-ish) rather than
        // return the write token, even when only the write slot is populated.
        let path = tmp_path("readbound");
        let write_tok = StoredToken {
            access_token: "WRITE-CAPABLE-TOKEN".into(),
            refresh_token: None,
            expires_at: None,
            scopes: None,
        };
        store_token_file(&path, "you@x.com", CredentialKind::Write, &write_tok).unwrap();

        let store = FileCredentialStore::new(1_i64, "you@x.com".into(), path.clone(), client());
        // Read slot is empty -> error, and it certainly never yields the write token.
        let err = store.token(1_i64).await;
        assert!(
            err.is_err(),
            "read-bound store must not read the write slot"
        );
        std::fs::remove_file(&path).ok();
    }

    #[tokio::test]
    async fn file_store_returns_valid_token() {
        let path = tmp_path("valid");
        let tok = StoredToken {
            access_token: "still-good".into(),
            refresh_token: Some("r".into()),
            expires_at: Some(Utc::now() + ChronoDuration::hours(1)),
            scopes: None,
        };
        store_token_file(&path, "you@x.com", CredentialKind::Read, &tok).unwrap();
        let store = FileCredentialStore::new(1_i64, "you@x.com".into(), path.clone(), client());
        let got = store.token(1_i64).await.unwrap();
        assert_eq!(got.access_token, "still-good");
        std::fs::remove_file(&path).ok();
    }

    // -----------------------------------------------------------------------
    // Age encryption at rest (hosted).
    // -----------------------------------------------------------------------
    //
    // Nearly everything below drives the identity explicitly through the
    // `_with` seams instead of the process env, because these tests share a
    // process with every other test in the crate and a global that decides
    // whether credentials get encrypted is not something to toggle under them.
    // Exactly two tests touch the env — the one that proves the var is wired up
    // at all, and the one that asserts what an UNSET var writes — and those two
    // hold `ENV_LOCK` against each other.

    /// Guards the two tests that mutate `SQUELCH_CRED_AGE_IDENTITY`. Other
    /// tests only ever read it (transparently, via the public entry points),
    /// and both encrypted and plaintext files load either way, so a concurrent
    /// reader observing a set var still passes.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// A fresh identity plus the file naming it, in the shape `age-keygen -o`
    /// writes: a comment line, then the key.
    fn identity_file(tag: &str) -> (PathBuf, AgeIdentity) {
        use age::secrecy::ExposeSecret;
        let generated = age::x25519::Identity::generate();
        let path = tmp_path(tag).with_extension("key");
        std::fs::write(
            &path,
            format!(
                "# created by a squelch test\n# public key: {}\n{}\n",
                generated.to_public(),
                generated.to_string().expose_secret()
            ),
        )
        .unwrap();
        let loaded = AgeIdentity::read(&path).unwrap();
        (path, loaded)
    }

    fn sealed_token(access: &str, refresh: &str) -> StoredToken {
        StoredToken {
            access_token: access.into(),
            refresh_token: Some(refresh.into()),
            expires_at: Some(Utc::now() + ChronoDuration::hours(1)),
            scopes: None,
        }
    }

    /// True if the raw file bytes carry `needle` anywhere. The assertion these
    /// tests actually care about is the NEGATIVE one: a sealed file must not
    /// contain a refresh token in the clear.
    fn bytes_contain(haystack: &[u8], needle: &str) -> bool {
        String::from_utf8_lossy(haystack).contains(needle)
    }

    #[test]
    fn a_configured_identity_seals_the_credentials_file() {
        let (key, id) = identity_file("seal");
        let path = tmp_path("seal-creds");
        let tok = sealed_token("sealed-access", "sealed-refresh");

        store_token_file_with(&path, "you@x.com", CredentialKind::Read, &tok, Some(&id)).unwrap();

        let raw = std::fs::read(&path).unwrap();
        assert!(
            raw.starts_with(AGE_ARMOR_BEGIN.as_bytes()),
            "a sealed host must write age armor, got: {:?}",
            String::from_utf8_lossy(&raw[..raw.len().min(40)])
        );
        assert!(
            !bytes_contain(&raw, "sealed-refresh"),
            "the refresh token must not survive anywhere in the file"
        );
        assert!(!bytes_contain(&raw, "sealed-access"));
        assert!(
            !bytes_contain(&raw, "you@x.com"),
            "even the slot keys are inside the ciphertext"
        );

        let back =
            load_token_file_with(&path, "you@x.com", CredentialKind::Read, Some(&id)).unwrap();
        assert_eq!(back, tok);

        std::fs::remove_file(&path).ok();
        std::fs::remove_file(&key).ok();
    }

    #[cfg(unix)]
    #[test]
    fn a_sealed_file_is_still_0600() {
        use std::os::unix::fs::PermissionsExt;
        let (key, id) = identity_file("sealmode");
        let path = tmp_path("sealmode-creds");
        store_token_file_with(
            &path,
            "you@x.com",
            CredentialKind::Read,
            &sealed_token("a", "r"),
            Some(&id),
        )
        .unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "mode was {:o}", mode & 0o777);
        std::fs::remove_file(&path).ok();
        std::fs::remove_file(&key).ok();
    }

    /// Migration read: a box that ran unencrypted and is later handed an
    /// identity must still open what it already has, and the very next write
    /// must seal it.
    #[test]
    fn a_legacy_plaintext_file_loads_under_an_identity_and_is_sealed_on_the_next_write() {
        let (key, id) = identity_file("migrate");
        let path = tmp_path("migrate-creds");
        let legacy = sealed_token("legacy-access", "legacy-refresh");

        // Written the way the daemon wrote it before any of this existed.
        store_token_file_with(&path, "you@x.com", CredentialKind::Read, &legacy, None).unwrap();
        assert!(std::fs::read_to_string(&path).unwrap().starts_with('{'));

        // Read under the identity: plaintext is recognized and returned.
        let back =
            load_token_file_with(&path, "you@x.com", CredentialKind::Read, Some(&id)).unwrap();
        assert_eq!(back, legacy);

        // The next write seals the WHOLE file, including the slot that was
        // already there and is not the one being written.
        let write_tok = sealed_token("write-access", "write-refresh");
        store_token_file_with(
            &path,
            "you@x.com",
            CredentialKind::Write,
            &write_tok,
            Some(&id),
        )
        .unwrap();
        let raw = std::fs::read(&path).unwrap();
        assert!(raw.starts_with(AGE_ARMOR_BEGIN.as_bytes()));
        assert!(!bytes_contain(&raw, "legacy-refresh"));
        assert!(!bytes_contain(&raw, "write-refresh"));

        assert_eq!(
            load_token_file_with(&path, "you@x.com", CredentialKind::Read, Some(&id)).unwrap(),
            legacy
        );
        assert_eq!(
            load_token_file_with(&path, "you@x.com", CredentialKind::Write, Some(&id)).unwrap(),
            write_tok
        );

        std::fs::remove_file(&path).ok();
        std::fs::remove_file(&key).ok();
    }

    /// The self-host guarantee: with no identity configured the file backend
    /// writes the exact plaintext JSON it always wrote, byte for byte.
    #[test]
    fn without_an_identity_the_bytes_are_the_plaintext_json_they_always_were() {
        let _g = ENV_LOCK.lock().unwrap();
        // SAFETY: guarded by ENV_LOCK — the only other test that WRITES this
        // var takes the same lock.
        unsafe {
            std::env::remove_var(CRED_AGE_IDENTITY_ENV);
        }

        let path = tmp_path("plainbytes");
        let tok = sealed_token("plain-access", "plain-refresh");
        store_token_file(&path, "you@x.com", CredentialKind::Read, &tok).unwrap();

        let mut expected = CredentialsFile::default();
        expected.slots.insert("you@x.com".to_string(), tok.clone());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            serde_json::to_string_pretty(&expected).unwrap(),
            "an unset identity must write the same bytes as before encryption existed"
        );
        assert_eq!(
            load_token_file(&path, "you@x.com", CredentialKind::Read).unwrap(),
            tok
        );
        std::fs::remove_file(&path).ok();
    }

    /// A sealed file on a box with no identity is a configuration mistake that
    /// must announce itself, not decode as gibberish and certainly not get
    /// overwritten in the clear.
    #[test]
    fn a_sealed_file_without_an_identity_refuses_by_name() {
        let (key, id) = identity_file("orphan");
        let path = tmp_path("orphan-creds");
        store_token_file_with(
            &path,
            "you@x.com",
            CredentialKind::Read,
            &sealed_token("orphan-access", "orphan-refresh"),
            Some(&id),
        )
        .unwrap();

        let err = load_token_file_with(&path, "you@x.com", CredentialKind::Read, None)
            .unwrap_err()
            .to_string();
        assert!(err.contains(CRED_AGE_IDENTITY_ENV), "{err}");
        assert!(err.contains(&path.display().to_string()), "{err}");
        assert!(!err.contains("orphan-refresh"), "{err}");

        std::fs::remove_file(&path).ok();
        std::fs::remove_file(&key).ok();
    }

    /// Every way an identity file can be wrong names the file, and none of them
    /// quietly degrade to plaintext.
    /// `AgeIdentity` is not `Debug` on purpose, so `unwrap_err` is unavailable
    /// here — which is the right trade.
    fn identity_err(path: &Path) -> String {
        match AgeIdentity::read(path) {
            Err(e) => e.to_string(),
            Ok(_) => panic!("that identity file should not have parsed"),
        }
    }

    #[test]
    fn a_bad_identity_file_is_loud_and_names_the_path() {
        let missing = tmp_path("nosuch").with_extension("key");
        let err = identity_err(&missing);
        assert!(err.contains(&missing.display().to_string()), "{err}");
        assert!(err.contains(CRED_AGE_IDENTITY_ENV), "{err}");

        let empty = tmp_path("emptykey").with_extension("key");
        std::fs::write(&empty, "# only a comment\n\n").unwrap();
        let err = identity_err(&empty);
        assert!(err.contains(&empty.display().to_string()), "{err}");
        assert!(err.contains("age-keygen"), "{err}");

        // A truncated key line: the parser's complaint is what tells an
        // operator the file is damaged rather than simply the wrong file.
        let truncated = tmp_path("truncatedkey").with_extension("key");
        std::fs::write(&truncated, "AGE-SECRET-KEY-1QQQQ\n").unwrap();
        let err = identity_err(&truncated);
        assert!(err.contains(&truncated.display().to_string()), "{err}");
        assert!(err.contains("malformed"), "{err}");

        for p in [&empty, &truncated] {
            std::fs::remove_file(p).ok();
        }
    }

    /// The wrong key is a refusal, not an empty slot map: a box restored with a
    /// stale identity must stop, not silently look like a mailbox with no
    /// credentials (which would send it off to re-authorize).
    #[test]
    fn a_file_sealed_to_a_stranger_does_not_open() {
        let (key_a, mine) = identity_file("mine");
        let (key_b, theirs) = identity_file("theirs");
        let path = tmp_path("stranger-creds");
        store_token_file_with(
            &path,
            "you@x.com",
            CredentialKind::Read,
            &sealed_token("stranger-access", "stranger-refresh"),
            Some(&theirs),
        )
        .unwrap();

        let err = load_token_file_with(&path, "you@x.com", CredentialKind::Read, Some(&mine))
            .unwrap_err()
            .to_string();
        assert!(err.contains(&path.display().to_string()), "{err}");
        assert!(err.contains(&key_a.display().to_string()), "{err}");
        assert!(!err.contains("stranger-refresh"), "{err}");

        for p in [&path, &key_a, &key_b] {
            std::fs::remove_file(p).ok();
        }
    }

    /// We only ever write armor, but a human recovering a box may drop a plain
    /// `age -e` binary file in place. That must read as an age file too.
    #[test]
    fn a_binary_age_file_is_recognized_as_well() {
        let (key, id) = identity_file("binary");
        let path = tmp_path("binary-creds");
        let tok = sealed_token("binary-access", "binary-refresh");
        let plaintext =
            credentials_file_plaintext("you@x.com", &[(CredentialKind::Read, tok.clone())])
                .unwrap();
        let binary = age::encrypt(&id.identity.to_public(), plaintext.as_bytes()).unwrap();
        write_private(&path, &binary).unwrap();

        assert!(is_age_file(&binary));
        assert_eq!(
            load_token_file_with(&path, "you@x.com", CredentialKind::Read, Some(&id)).unwrap(),
            tok
        );

        std::fs::remove_file(&path).ok();
        std::fs::remove_file(&key).ok();
    }

    /// The provisioning direction end to end: a control plane holding only the
    /// RECIPIENT produces bytes this backend opens, in the exact on-disk shape.
    /// If these two halves ever drift, a hosted tenant boots with an empty slot
    /// map instead of an error, which is why this is a test and not a comment.
    #[test]
    fn what_a_provisioner_seals_is_what_the_daemon_opens() {
        let (key, id) = identity_file("provision");
        let recipient = id.identity.to_public().to_string();
        let tok = sealed_token("hosted-access", "hosted-refresh");

        let plaintext =
            credentials_file_plaintext("tenant@x.com", &[(CredentialKind::Read, tok.clone())])
                .unwrap();
        let armored = seal_credentials_for_recipient(&recipient, plaintext.as_bytes()).unwrap();
        assert!(armored.starts_with(AGE_ARMOR_BEGIN));
        assert!(!armored.contains("hosted-refresh"));

        // Warden writes the ciphertext down verbatim, with the same 0600
        // discipline this module uses everywhere.
        let path = tmp_path("provisioned-creds");
        write_private(&path, armored.as_bytes()).unwrap();

        assert_eq!(
            load_token_file_with(&path, "tenant@x.com", CredentialKind::Read, Some(&id)).unwrap(),
            tok
        );

        std::fs::remove_file(&path).ok();
        std::fs::remove_file(&key).ok();
    }

    #[test]
    fn a_recipient_typo_fails_where_the_plaintext_can_still_be_dropped() {
        let err = seal_credentials_for_recipient("age1-definitely-not-a-key", b"{}")
            .unwrap_err()
            .to_string();
        assert!(err.contains("age1"), "{err}");
    }

    /// A one-shot token endpoint on loopback. Never Google: nothing in this
    /// crate's tests is allowed to touch the real one.
    fn scripted_token_endpoint(body: &str) -> String {
        use std::io::{Read as _, Write as _};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let raw = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
             Connection: close\r\n\r\n{body}",
            body.len()
        );
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut scratch = [0u8; 4096];
                let _ = stream.read(&mut scratch);
                let _ = stream.write_all(raw.as_bytes());
                let _ = stream.flush();
            }
        });
        base
    }

    /// The rewrite that happens when a token expires mid-run must stay sealed.
    /// That path is `validate_or_refresh`'s persist closure, which is exactly
    /// `store_token_file` — reproduced here rather than driven through
    /// [`FileCredentialStore`] because `validate_or_refresh` pins Google's token
    /// URL and no test may call it. The refresh half is still real: the fresh
    /// token comes off a scripted socket through the same parser the daemon
    /// uses, so what lands on disk is a genuine refresh response.
    #[test]
    fn a_refresh_driven_rewrite_stays_sealed() {
        let (key, id) = identity_file("refresh");
        let path = tmp_path("refresh-creds");
        let stale = StoredToken {
            access_token: "stale-access".into(),
            refresh_token: Some("old-refresh".into()),
            expires_at: Some(Utc::now() - ChronoDuration::minutes(5)),
            scopes: None,
        };
        store_token_file_with(&path, "you@x.com", CredentialKind::Read, &stale, Some(&id)).unwrap();

        let base = scripted_token_endpoint(
            r#"{"access_token":"fresh-access","token_type":"Bearer","expires_in":3600,"refresh_token":"fresh-refresh"}"#,
        );
        let fresh = refresh_stored_token_detailed_at(
            &client(),
            "old-refresh",
            &format!("{base}/token"),
            Duration::from_secs(5),
        )
        .unwrap()
        .token;
        assert_eq!(fresh.access_token, "fresh-access");

        store_token_file_with(&path, "you@x.com", CredentialKind::Read, &fresh, Some(&id)).unwrap();

        let raw = std::fs::read(&path).unwrap();
        assert!(raw.starts_with(AGE_ARMOR_BEGIN.as_bytes()));
        assert!(
            !bytes_contain(&raw, "fresh-refresh"),
            "a refreshed refresh token must not land in the clear"
        );
        assert!(!bytes_contain(&raw, "fresh-access"));
        assert_eq!(
            load_token_file_with(&path, "you@x.com", CredentialKind::Read, Some(&id))
                .unwrap()
                .access_token,
            "fresh-access"
        );

        std::fs::remove_file(&path).ok();
        std::fs::remove_file(&key).ok();
    }

    /// The env var is the whole switch: set it and the public entry points seal,
    /// unset it and they do not. Blank values read as unset so a stray
    /// `SQUELCH_CRED_AGE_IDENTITY=` in an env file cannot wedge a daemon.
    #[test]
    fn the_env_var_is_what_turns_sealing_on() {
        // A hand-rolled runtime rather than `#[tokio::test]`: the env guard has
        // to be held across the store's `.await`, and holding a lock across an
        // await point in an async fn is exactly what clippy asks you not to do.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _g = ENV_LOCK.lock().unwrap();
        let (key, _) = identity_file("wiring");
        let path = tmp_path("wiring-creds");
        let tok = sealed_token("wired-access", "wired-refresh");

        // SAFETY: guarded by ENV_LOCK.
        unsafe {
            std::env::set_var(CRED_AGE_IDENTITY_ENV, &key);
        }
        let stored = store_token_file(&path, "you@x.com", CredentialKind::Read, &tok);
        let loaded = load_token_file(&path, "you@x.com", CredentialKind::Read);
        // The store's own read path, since that is what actually runs in the
        // daemon: a sealed file must serve a live token like any other.
        let store = FileCredentialStore::new(7_i64, "you@x.com".into(), path.clone(), client());
        let via_store = rt.block_on(store.token(7_i64));
        // Restore BEFORE asserting: a panic here must not leave the var set for
        // every other test in this process.
        unsafe {
            std::env::remove_var(CRED_AGE_IDENTITY_ENV);
        }

        stored.unwrap();
        assert_eq!(loaded.unwrap(), tok);
        assert_eq!(via_store.unwrap().access_token, "wired-access");
        let raw = std::fs::read(&path).unwrap();
        assert!(raw.starts_with(AGE_ARMOR_BEGIN.as_bytes()));
        assert!(!bytes_contain(&raw, "wired-refresh"));

        // With the var gone, that same file refuses by name rather than being
        // read as JSON or replaced in the clear.
        let err = load_token_file(&path, "you@x.com", CredentialKind::Read)
            .unwrap_err()
            .to_string();
        assert!(err.contains(CRED_AGE_IDENTITY_ENV), "{err}");

        // A blank value is "not configured", not "identity at the empty path".
        let blank = tmp_path("blank-creds");
        // SAFETY: guarded by ENV_LOCK.
        unsafe {
            std::env::set_var(CRED_AGE_IDENTITY_ENV, "   ");
        }
        let stored = store_token_file(&blank, "you@x.com", CredentialKind::Read, &tok);
        unsafe {
            std::env::remove_var(CRED_AGE_IDENTITY_ENV);
        }
        stored.unwrap();
        assert!(std::fs::read_to_string(&blank).unwrap().starts_with('{'));

        // And a path that is set but wrong is fatal, never a plaintext write.
        let doomed = tmp_path("doomed-creds");
        let nowhere = tmp_path("nowhere").with_extension("key");
        // SAFETY: guarded by ENV_LOCK.
        unsafe {
            std::env::set_var(CRED_AGE_IDENTITY_ENV, &nowhere);
        }
        let stored = store_token_file(&doomed, "you@x.com", CredentialKind::Read, &tok);
        unsafe {
            std::env::remove_var(CRED_AGE_IDENTITY_ENV);
        }
        let err = stored.unwrap_err().to_string();
        assert!(err.contains(&nowhere.display().to_string()), "{err}");
        assert!(
            !doomed.exists(),
            "a missing identity must abort the write, not fall back to plaintext"
        );

        for p in [&path, &key, &blank] {
            std::fs::remove_file(p).ok();
        }
    }

    // -----------------------------------------------------------------------
    // Recorded scopes and the calendar resolver.
    // -----------------------------------------------------------------------

    const EVENTS: &str = crate::config::CALENDAR_EVENTS_SCOPE;
    const LIST: &str = crate::config::CALENDAR_LIST_SCOPE;
    const READONLY: &str = crate::config::GMAIL_READONLY_SCOPE;

    fn scoped(access: &str, scopes: Option<&[&str]>) -> StoredToken {
        StoredToken {
            access_token: access.into(),
            refresh_token: Some("r".into()),
            expires_at: Some(Utc::now() + ChronoDuration::hours(1)),
            scopes: scopes.map(|s| s.iter().map(|s| s.to_string()).collect()),
        }
    }

    /// Every stored token written before `scopes` existed must still load, in
    /// every backend's shape: the bare JSON the keyring holds and the slot map
    /// the file holds. Unknown scopes read as "calendar not granted".
    #[test]
    fn an_old_stored_token_without_scopes_still_loads() {
        let old = r#"{"access_token":"a","refresh_token":"r","expires_at":"2026-01-01T00:00:00Z"}"#;
        let back = StoredToken::from_json(old).unwrap();
        assert_eq!(back.scopes, None);
        assert!(!back.grants_calendar());
        assert_eq!(back.refresh_token.as_deref(), Some("r"));

        let path = tmp_path("oldshape");
        std::fs::write(&path, format!(r#"{{"slots":{{"you@x.com":{old}}}}}"#)).unwrap();
        let loaded = load_token_file_with(&path, "you@x.com", CredentialKind::Read, None).unwrap();
        assert_eq!(loaded.scopes, None);
        std::fs::remove_file(&path).ok();

        // And a token with nothing recorded writes the bytes it always did.
        assert!(!scoped("a", None).to_json().unwrap().contains("scopes"));
        let with = scoped("a", Some(&[EVENTS]));
        assert_eq!(
            StoredToken::from_json(&with.to_json().unwrap()).unwrap(),
            with
        );
    }

    /// A refresh response that omits `scope` keeps the scopes on record; one
    /// that names them replaces them. Driven through the real refresh-and-
    /// persist path against a scripted token endpoint, with an expired token,
    /// so what is asserted is what lands in the backend.
    #[test]
    fn recorded_scopes_survive_a_refresh_that_omits_scope() {
        let path = tmp_path("refresh-scopes");
        let mut stale = scoped("stale", Some(&[READONLY, EVENTS]));
        stale.expires_at = Some(Utc::now() - ChronoDuration::minutes(5));
        store_token_file_with(&path, "you@x.com", CredentialKind::Read, &stale, None).unwrap();

        let base = scripted_token_endpoint(
            r#"{"access_token":"fresh-1","token_type":"Bearer","expires_in":3600}"#,
        );
        let got = validate_or_refresh_at(
            stale.clone(),
            &client(),
            &format!("{base}/token"),
            Duration::from_secs(5),
            |fresh| store_token_file_with(&path, "you@x.com", CredentialKind::Read, fresh, None),
        )
        .unwrap();
        assert_eq!(got.access_token, "fresh-1");
        let landed = load_token_file_with(&path, "you@x.com", CredentialKind::Read, None).unwrap();
        assert_eq!(landed.access_token, "fresh-1");
        assert_eq!(
            landed.scopes, stale.scopes,
            "an omitted scope keeps the record"
        );
        assert!(landed.grants_calendar());
        assert_eq!(landed.refresh_token.as_deref(), Some("r"));

        // A refresh that NAMES scopes is the newer answer: calendar revoked at
        // Google stops reading as granted.
        let mut stale = landed;
        stale.expires_at = Some(Utc::now() - ChronoDuration::minutes(5));
        let base = scripted_token_endpoint(&format!(
            r#"{{"access_token":"fresh-2","token_type":"Bearer","expires_in":3600,"scope":"{READONLY}"}}"#
        ));
        validate_or_refresh_at(
            stale,
            &client(),
            &format!("{base}/token"),
            Duration::from_secs(5),
            |fresh| store_token_file_with(&path, "you@x.com", CredentialKind::Read, fresh, None),
        )
        .unwrap();
        let landed = load_token_file_with(&path, "you@x.com", CredentialKind::Read, None).unwrap();
        assert_eq!(landed.scopes, Some(vec![READONLY.to_string()]));
        assert!(!landed.grants_calendar());
        std::fs::remove_file(&path).ok();
    }

    /// THE RESOLVER'S ORDER: the Calendar slot when it grants calendar, then
    /// the Read slot when IT does, then nothing. Unknown scopes never answer.
    #[test]
    fn the_calendar_slot_wins_then_a_calendar_scoped_read_slot() {
        let cal = || scoped("CAL", Some(&[EVENTS, LIST, READONLY]));
        let read_cal = || scoped("READ-WITH-CAL", Some(&[READONLY, EVENTS]));
        let read_plain = || scoped("READ", Some(&[READONLY]));
        let pick = |c, r| pick_calendar_slot(c, r).map(|(k, t)| (k, t.access_token));

        assert_eq!(
            pick(Some(cal()), Some(read_cal())),
            Some((CredentialKind::Calendar, "CAL".into()))
        );
        assert_eq!(
            pick(Some(cal()), None),
            Some((CredentialKind::Calendar, "CAL".into()))
        );
        assert_eq!(
            pick(None, Some(read_cal())),
            Some((CredentialKind::Read, "READ-WITH-CAL".into()))
        );
        // A Read slot without the events scope, or with unknown scopes, is
        // not a calendar grant (the list scope alone is not one either).
        assert_eq!(pick(None, Some(read_plain())), None);
        assert_eq!(pick(None, Some(scoped("OLD", None))), None);
        assert_eq!(pick(None, Some(scoped("LIST", Some(&[LIST])))), None);
        assert_eq!(pick(None, None), None);
        // A Calendar slot whose latest refresh no longer names events falls
        // through to the Read slot rather than answering.
        assert_eq!(
            pick(
                Some(scoped("CAL-REVOKED", Some(&[READONLY]))),
                Some(read_cal())
            ),
            Some((CredentialKind::Read, "READ-WITH-CAL".into()))
        );
        assert_eq!(
            pick(
                Some(scoped("CAL-REVOKED", Some(&[READONLY]))),
                Some(read_plain())
            ),
            None
        );
    }

    /// The resolver against a real file backend: it reads the slots as the
    /// daemon stores them, never consults Write, and hands back a live token.
    ///
    /// A hand-rolled runtime holding `ENV_LOCK`, like the env-wiring test: the
    /// resolver goes through the public entry points, which read
    /// `SQUELCH_CRED_AGE_IDENTITY`, and a neighbour pointing it at a missing
    /// file mid-run would read as "no slot" here.
    #[test]
    fn the_calendar_resolver_reads_the_file_backend() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _g = ENV_LOCK.lock().unwrap();
        rt.block_on(calendar_resolver_scenario());
    }

    /// `granted` is cached, but only while the credentials file is the one
    /// it read: a file whose contents change under the same mtime is still
    /// answered from the cache (the hit), and any new mtime is read again.
    #[test]
    fn the_granted_cache_holds_until_the_file_changes() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _g = ENV_LOCK.lock().unwrap();
        rt.block_on(async {
            let path = tmp_path("granted-cache");
            let resolver = CalendarCredentials::new(
                3_i64,
                "you@x.com".into(),
                CredentialBackend::File,
                path.clone(),
                client(),
            );
            let write = |token: StoredToken| {
                store_token_file(&path, "you@x.com", CredentialKind::Read, &token).unwrap()
            };
            write(scoped("READ-WITH-CAL", Some(&[READONLY, EVENTS])));
            let first = std::fs::metadata(&path).unwrap().modified().unwrap();
            assert!(resolver.granted().await);

            // The grant goes, but the mtime is put back: still the cache.
            write(scoped("READ", Some(&[READONLY])));
            std::fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(first)
                .unwrap();
            assert!(resolver.granted().await, "answered from the cache");

            // A new mtime is a new answer at once, well inside the TTL.
            std::fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(first + Duration::from_secs(1))
                .unwrap();
            assert!(!resolver.granted().await, "a changed file is read again");
            std::fs::remove_file(&path).ok();
        });
    }

    async fn calendar_resolver_scenario() {
        let path = tmp_path("calendar-resolver");
        let resolver = CalendarCredentials::new(
            3_i64,
            "you@x.com".into(),
            CredentialBackend::File,
            path.clone(),
            client(),
        );
        assert!(!resolver.granted().await, "an empty file grants nothing");
        assert!(resolver.calendar_token(3_i64).await.unwrap().is_none());
        assert!(resolver.token(3_i64).await.is_err());

        // A calendar-scoped WRITE slot is never the answer.
        store_tokens_backend(
            CredentialBackend::File,
            &path,
            "you@x.com",
            &[
                (CredentialKind::Write, scoped("WRITE", Some(&[EVENTS]))),
                (CredentialKind::Read, scoped("READ", Some(&[READONLY]))),
            ],
        )
        .unwrap();
        assert!(!resolver.granted().await);

        // The hosted shape: one token, calendar on the Read slot.
        store_token_file(
            &path,
            "you@x.com",
            CredentialKind::Read,
            &scoped("READ-WITH-CAL", Some(&[READONLY, EVENTS])),
        )
        .unwrap();
        assert!(resolver.granted().await);
        assert_eq!(
            resolver
                .calendar_token(3_i64)
                .await
                .unwrap()
                .unwrap()
                .access_token,
            "READ-WITH-CAL"
        );

        // The self-host shape: the Calendar slot wins.
        store_token_file(
            &path,
            "you@x.com",
            CredentialKind::Calendar,
            &scoped("CAL", Some(&[EVENTS])),
        )
        .unwrap();
        assert_eq!(resolver.token(3_i64).await.unwrap().access_token, "CAL");
        assert!(
            resolver.calendar_token(4_i64).await.is_err(),
            "bound to one account"
        );
        std::fs::remove_file(&path).ok();
    }

    // --- concurrent writers --------------------------------------------------

    /// A credentials file whose path carries this marker gets a short pause
    /// between its read and its write and between truncate and write, so the
    /// race below is hit on every run rather than on a lucky schedule.
    pub(super) const RACE_PAUSE_MARKER: &str = "race-pause";

    /// Many writers at once, of different sizes, to different slots and to one
    /// shared slot, with a reader decoding the file throughout. The file must
    /// decode every time it is read, each writer's own slot must end on the
    /// last value that writer wrote (no update lost to another writer's
    /// read-modify-write), and the shared slot must hold some writer's last
    /// value. `identity` sealed or not: hosted files are age-sealed.
    fn concurrent_writers_scenario(tag: &str, identity: Option<&AgeIdentity>) {
        const ROUNDS: usize = 12;
        let path = tmp_path(&format!("{RACE_PAUSE_MARKER}-{tag}"));
        let value = |writer: usize, round: usize| {
            // Sizes that differ between writers and rounds, so a torn file
            // would show one writer's bytes followed by another's tail.
            let pad = "x".repeat((writer * 211 + round * 97) % 900);
            format!("w{writer}-r{round}-{pad}")
        };
        let token = |access: String| StoredToken {
            access_token: access,
            refresh_token: Some("r".into()),
            expires_at: None,
            scopes: None,
        };
        let owned = [
            ("you@x.com", CredentialKind::Read),
            ("you@x.com", CredentialKind::Write),
            ("you@x.com", CredentialKind::Calendar),
        ];
        let shared = ("other@x.com", CredentialKind::Read);
        let done = std::sync::atomic::AtomicBool::new(false);
        let reads = std::sync::atomic::AtomicUsize::new(0);
        std::thread::scope(|scope| {
            let mut writers = Vec::new();
            for writer in 0..6 {
                let (email, kind) = owned.get(writer).copied().unwrap_or(shared);
                let path = &path;
                writers.push(scope.spawn(move || {
                    for round in 0..ROUNDS {
                        store_token_file_with(
                            path,
                            email,
                            kind,
                            &token(value(writer, round)),
                            identity,
                        )
                        .unwrap();
                    }
                }));
            }
            scope.spawn(|| {
                while !done.load(std::sync::atomic::Ordering::Relaxed) {
                    CredentialsFile::read(&path, identity)
                        .expect("the credentials file decodes whenever it is read");
                    reads.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
            });
            for writer in writers {
                writer.join().unwrap();
            }
            done.store(true, std::sync::atomic::Ordering::Relaxed);
        });
        assert!(reads.load(std::sync::atomic::Ordering::Relaxed) > 0);
        let file = CredentialsFile::read(&path, identity).unwrap();
        for (writer, (email, kind)) in owned.iter().enumerate() {
            assert_eq!(
                file.slots[&kind.slot_key(email)].access_token,
                value(writer, ROUNDS - 1),
                "writer {writer}'s last update was lost"
            );
        }
        let last: Vec<String> = (3..6).map(|w| value(w, ROUNDS - 1)).collect();
        assert!(last.contains(&file.slots[&shared.1.slot_key(shared.0)].access_token));
        // No temp file is left behind.
        let dir = path.parent().unwrap();
        let stem = path.file_name().unwrap().to_string_lossy().to_string();
        let strays = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                name.starts_with(&stem) && name != stem
            })
            .count();
        assert_eq!(strays, 0, "temp files left beside the credentials file");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn concurrent_writers_never_tear_or_lose_the_plain_file() {
        concurrent_writers_scenario("plain", None);
    }

    #[test]
    fn concurrent_writers_never_tear_or_lose_the_sealed_file() {
        let (key, identity) = identity_file("race-key");
        concurrent_writers_scenario("sealed", Some(&identity));
        std::fs::remove_file(&key).ok();
    }

    /// Every write gets a temp name of its own, so no two writers, in this
    /// process or another, ever open one temp file.
    #[test]
    fn every_write_has_its_own_temp_file() {
        let path = Path::new("/tmp/creds/credentials.json");
        let (a, b) = (unique_temp_path(path), unique_temp_path(path));
        assert_ne!(a, b);
        for tmp in [&a, &b] {
            assert_eq!(
                tmp.parent(),
                path.parent(),
                "same directory: rename stays atomic"
            );
            assert!(
                tmp.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("credentials.json.tmp.")
            );
        }
    }
}
