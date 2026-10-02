//! Auth for the agent door (`/mcp`): MCP-spec OAuth with the daemon as a pure
//! RESOURCE SERVER.
//!
//! An external authorization server (WorkOS AuthKit) issues RS256 access
//! tokens; this module only verifies them, locally, against the issuer's JWKS.
//! It never issues anything, never talks to the authorization server about a
//! specific token, and never sees the user's password or Google credential.
//!
//! Three modes, chosen once at startup from env ([`AgentDoorAuth::from_env`]):
//!
//! - **Open** (neither issuer nor public URL set): today's self-host door. No
//!   auth layer at all; the only guard is rmcp's Host allow-list.
//! - **OAuth** (both set): every `/mcp` request needs a valid bearer JWT.
//! - **Misconfigured** (exactly one set, or a value that does not parse): the
//!   door answers 503 to everything. A half-configured daemon must never fall
//!   back to Open, because the most likely way to get here is a hosted pod
//!   whose second variable went missing, and that pod's `/mcp` is published.
//!
//! The rule that matters most is the MAILBOX BINDING (contract I1). One
//! wildcard resource indicator covers the whole fleet, so any Passband user can
//! obtain a correctly signed token whose `aud` names another tenant. `aud` says
//! "this token is for a Passband daemon"; only the `email` claim says WHOSE.
//! [`Verifier::verify`] therefore requires `email` to equal this daemon's own
//! account email, and refuses a token that has none.
//!
//! Logging (contract I6): refusals are logged as a [`Refusal`] category and
//! nothing else. No token, no claim value, no email address ever reaches a log
//! line or a response body.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::Response;
use jsonwebtoken::jwk::{Jwk, JwkSet};
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use serde::Deserialize;
use tokio::sync::Mutex;

/// The authorization server's issuer URL, e.g. `https://foo.authkit.app`.
pub const ENV_MCP_OAUTH_ISSUER: &str = "SQUELCH_MCP_OAUTH_ISSUER";
/// The canonical resource URL of THIS daemon's agent door, e.g.
/// `https://alice.passband.email/mcp`. It is the expected `aud`, the
/// `resource` in the protected-resource metadata, and the source of the public
/// hostname added to the rmcp Host allow-list.
pub const ENV_MCP_PUBLIC_URL: &str = "SQUELCH_MCP_PUBLIC_URL";
/// Optional JWKS URL override. Default: `<issuer>/oauth2/jwks` (AuthKit's).
pub const ENV_MCP_OAUTH_JWKS_URL: &str = "SQUELCH_MCP_OAUTH_JWKS_URL";

/// Where the protected-resource metadata lives, relative to the public origin.
/// RFC 9728 §3.1: the well-known suffix is inserted between the host and the
/// resource's path, so `https://h/mcp` has its metadata at
/// `https://h/.well-known/oauth-protected-resource/mcp`.
pub const PROTECTED_RESOURCE_METADATA_PATH: &str = "/.well-known/oauth-protected-resource/mcp";
/// The path-less form, served too: some clients probe the root variant.
pub const PROTECTED_RESOURCE_METADATA_ROOT_PATH: &str = "/.well-known/oauth-protected-resource";

/// Presented tokens longer than this are refused before any parsing, base64
/// decoding or key lookup. A real AuthKit access token is well under 2 KiB;
/// the cap bounds the work an unauthenticated caller can make us do.
pub const MAX_TOKEN_BYTES: usize = 8 * 1024;

/// Clock-skew allowance on `exp` and `nbf`.
pub const LEEWAY_SECS: u64 = 60;

/// The JWKS is fetched at most once per this window, whatever happens. An
/// attacker spraying tokens with random `kid`s must not be able to turn each
/// request into an outbound fetch.
pub const JWKS_MIN_REFETCH: Duration = Duration::from_secs(60);

/// Bound on a JWKS fetch, connect through body.
const JWKS_TIMEOUT: Duration = Duration::from_secs(5);

/// A JWKS document bigger than this is not one we will parse. AuthKit's is a
/// few hundred bytes.
const JWKS_MAX_BYTES: usize = 256 * 1024;

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// How the agent door authenticates. Decided once, at startup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentDoorAuth {
    /// No auth layer. Self-host behaviour, byte-for-byte (contract I4).
    Open,
    /// Bearer JWTs from the configured issuer, bound to this mailbox.
    OAuth(OAuthConfig),
    /// Half-set or unparseable config. `/mcp` answers 503; nothing is served.
    Misconfigured(MisconfiguredReason),
}

/// Validated OAuth-mode settings. Only constructible through
/// [`AgentDoorAuth::from_values`], so every instance has passed URL checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthConfig {
    issuer: String,
    public_url: String,
    jwks_url: String,
    /// `scheme://host[:port]` of the public URL, for the metadata link.
    public_origin: String,
    /// Host allow-list entries for the public URL: the bare host, plus
    /// `host:port` when the URL names a port.
    public_hosts: Vec<String>,
}

impl OAuthConfig {
    /// The issuer exactly as configured (trimmed). Published in metadata.
    pub fn issuer(&self) -> &str {
        &self.issuer
    }
    /// The canonical resource URL exactly as configured (trimmed).
    pub fn public_url(&self) -> &str {
        &self.public_url
    }
    /// The JWKS URL in use (override or `<issuer>/oauth2/jwks`).
    pub fn jwks_url(&self) -> &str {
        &self.jwks_url
    }
    /// The absolute URL of the protected-resource metadata document, as it
    /// appears in `WWW-Authenticate`.
    pub fn resource_metadata_url(&self) -> String {
        format!("{}{PROTECTED_RESOURCE_METADATA_PATH}", self.public_origin)
    }
    /// What OAuth mode adds to the rmcp Host allow-list.
    pub fn public_hosts(&self) -> &[String] {
        &self.public_hosts
    }
}

/// Why the door is misconfigured. A closed set, safe to log: it names which
/// setting is wrong, never its value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MisconfiguredReason {
    /// The issuer is set but the public URL is not.
    IssuerWithoutPublicUrl,
    /// The public URL is set but the issuer is not.
    PublicUrlWithoutIssuer,
    /// The issuer is not an acceptable URL.
    InvalidIssuer,
    /// The public URL is not an acceptable URL.
    InvalidPublicUrl,
    /// The JWKS override is not an acceptable URL.
    InvalidJwksUrl,
    /// OAuth is configured but the daemon has no real account email to bind
    /// tokens to (unset, or the `me@localhost` placeholder).
    NoAccountEmail,
}

impl MisconfiguredReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::IssuerWithoutPublicUrl => {
                "SQUELCH_MCP_OAUTH_ISSUER is set but SQUELCH_MCP_PUBLIC_URL is not"
            }
            Self::PublicUrlWithoutIssuer => {
                "SQUELCH_MCP_PUBLIC_URL is set but SQUELCH_MCP_OAUTH_ISSUER is not"
            }
            Self::InvalidIssuer => "SQUELCH_MCP_OAUTH_ISSUER is not a valid https URL",
            Self::InvalidPublicUrl => "SQUELCH_MCP_PUBLIC_URL is not a valid https URL",
            Self::InvalidJwksUrl => "SQUELCH_MCP_OAUTH_JWKS_URL is not a valid https URL",
            Self::NoAccountEmail => {
                "agent sign-in needs this daemon's real account email (SQUELCH_ACCOUNT_EMAIL), not the default"
            }
        }
    }
}

impl std::fmt::Display for MisconfiguredReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Whether an account email can anchor the mailbox binding: non-blank, shaped
/// like an address, and not the placeholder default.
fn bindable_account_email(e: &str) -> bool {
    let e = e.trim();
    !e.is_empty()
        && e.contains('@')
        && !e.eq_ignore_ascii_case(squelch_core::config::DEFAULT_ACCOUNT_EMAIL)
}

/// Blank (or all-whitespace) is unset, matching every other squelch env var.
fn non_blank(v: Option<&str>) -> Option<&str> {
    v.map(str::trim).filter(|s| !s.is_empty())
}

/// Accept `https` anywhere, `http` only for loopback hosts (tests and local
/// runs). No userinfo, no fragment: a credential in a URL we publish in
/// metadata would be a leak, and RFC 8707 forbids a fragment in a resource.
fn acceptable_url(raw: &str) -> Option<url::Url> {
    let u = url::Url::parse(raw).ok()?;
    let host = u.host_str()?;
    let loopback = matches!(host, "localhost" | "127.0.0.1" | "[::1]" | "::1");
    let scheme_ok = u.scheme() == "https" || (u.scheme() == "http" && loopback);
    let clean = u.username().is_empty() && u.password().is_none() && u.fragment().is_none();
    (scheme_ok && clean).then_some(u)
}

/// Contract: `iss` and `aud` compare exactly after trimming ONE trailing `/`.
fn trim_one_slash(s: &str) -> &str {
    s.strip_suffix('/').unwrap_or(s)
}

impl AgentDoorAuth {
    /// Read the mode from the process environment. Blank means unset.
    pub fn from_env() -> Self {
        let issuer = std::env::var(ENV_MCP_OAUTH_ISSUER).ok();
        let public = std::env::var(ENV_MCP_PUBLIC_URL).ok();
        let jwks = std::env::var(ENV_MCP_OAUTH_JWKS_URL).ok();
        Self::from_values(issuer.as_deref(), public.as_deref(), jwks.as_deref())
    }

    /// The mode for a startup line, shared by both binaries so they say the
    /// same thing. The public URL is public (the metadata publishes it); a
    /// misconfiguration names the setting at fault, never its value.
    pub fn describe(&self) -> String {
        match self {
            Self::Open => "open, no agent sign-in".to_string(),
            Self::OAuth(c) => format!("OAuth, public URL {}", c.public_url),
            Self::Misconfigured(why) => format!("MISCONFIGURED, answering 503: {why}"),
        }
    }

    /// Bind the mode to the mailbox this daemon serves.
    ///
    /// OAuth binds every token to the account email (I1), so OAuth with no
    /// real address to bind to is Misconfigured: an unset value, or the
    /// [`squelch_core::config::DEFAULT_ACCOUNT_EMAIL`] placeholder a bare
    /// config falls back to. Open and Misconfigured pass through unchanged.
    ///
    /// [`crate::agent_door_router`] applies this itself, so no host can skip
    /// it; callers that describe the mode (a startup line, the console) call
    /// it too, so what they say matches what the router does.
    pub fn for_account(self, account_email: &str) -> Self {
        match self {
            Self::OAuth(_) if !bindable_account_email(account_email) => {
                Self::Misconfigured(MisconfiguredReason::NoAccountEmail)
            }
            other => other,
        }
    }

    /// Pure constructor, so tests never touch process env.
    ///
    /// Mode is decided by the issuer and public URL alone. A JWKS override
    /// with neither of them set is ignored (Open): it carries no intent to
    /// require auth, and the contract defines Open as "neither set".
    pub fn from_values(
        issuer: Option<&str>,
        public_url: Option<&str>,
        jwks_url: Option<&str>,
    ) -> Self {
        let (issuer, public_url) = match (non_blank(issuer), non_blank(public_url)) {
            (None, None) => return Self::Open,
            (Some(_), None) => {
                return Self::Misconfigured(MisconfiguredReason::IssuerWithoutPublicUrl);
            }
            (None, Some(_)) => {
                return Self::Misconfigured(MisconfiguredReason::PublicUrlWithoutIssuer);
            }
            (Some(i), Some(p)) => (i, p),
        };
        if acceptable_url(issuer).is_none() {
            return Self::Misconfigured(MisconfiguredReason::InvalidIssuer);
        }
        let Some(public) = acceptable_url(public_url) else {
            return Self::Misconfigured(MisconfiguredReason::InvalidPublicUrl);
        };
        let jwks_url = match non_blank(jwks_url) {
            Some(j) => match acceptable_url(j) {
                Some(_) => j.to_string(),
                None => return Self::Misconfigured(MisconfiguredReason::InvalidJwksUrl),
            },
            None => format!("{}/oauth2/jwks", trim_one_slash(issuer)),
        };

        // `acceptable_url` guarantees a host.
        let host = public.host_str().unwrap_or_default().to_string();
        let mut public_hosts = vec![host.clone()];
        let mut public_origin = format!("{}://{host}", public.scheme());
        if let Some(port) = public.port() {
            public_hosts.push(format!("{host}:{port}"));
            public_origin.push_str(&format!(":{port}"));
        }

        Self::OAuth(OAuthConfig {
            issuer: issuer.to_string(),
            public_url: public_url.to_string(),
            jwks_url,
            public_origin,
            public_hosts,
        })
    }
}

// ---------------------------------------------------------------------------
// Refusals
// ---------------------------------------------------------------------------

/// Why a request was refused. A CLOSED set used only for logging (I6): it
/// names the failed check and carries no value from the request. The HTTP
/// response never says which one it was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// No `Authorization` header, or not a `Bearer` one.
    NoToken,
    /// The presented token exceeds [`MAX_TOKEN_BYTES`].
    Oversize,
    /// Not a parseable JWT (bad segments, base64, JSON, or claim types).
    Malformed,
    /// Header `alg` is not RS256 (covers `none` and HS* confusion).
    Algorithm,
    /// Header carries `crit` (RFC 7515 §4.1.11): extensions we do not
    /// understand and so must not accept.
    CriticalHeader,
    /// Header has no `kid`.
    MissingKid,
    /// No key in the JWKS has this `kid`, even after a permitted refresh.
    UnknownKey,
    /// The signature does not verify under the key named by `kid`.
    BadSignature,
    /// `iss` missing or not our issuer.
    Issuer,
    /// `aud` missing or does not contain our public URL.
    Audience,
    /// `exp` missing.
    MissingExp,
    /// `exp` passed (beyond leeway).
    Expired,
    /// `nbf` in the future (beyond leeway).
    NotYetValid,
    /// No `email` claim.
    MissingEmail,
    /// `email` is another mailbox (I1).
    WrongMailbox,
    /// The JWKS has never been fetched successfully, so nothing can verify.
    JwksUnavailable,
    /// The door is misconfigured; nothing is served.
    Misconfigured,
}

impl Refusal {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NoToken => "no_token",
            Self::Oversize => "oversize",
            Self::Malformed => "malformed",
            Self::Algorithm => "algorithm",
            Self::CriticalHeader => "critical_header",
            Self::MissingKid => "missing_kid",
            Self::UnknownKey => "unknown_key",
            Self::BadSignature => "bad_signature",
            Self::Issuer => "issuer",
            Self::Audience => "audience",
            Self::MissingExp => "missing_exp",
            Self::Expired => "expired",
            Self::NotYetValid => "not_yet_valid",
            Self::MissingEmail => "missing_email",
            Self::WrongMailbox => "wrong_mailbox",
            Self::JwksUnavailable => "jwks_unavailable",
            Self::Misconfigured => "misconfigured",
        }
    }

    /// Every category, in a fixed order: the refusal log's counters.
    pub const ALL: [Refusal; 17] = [
        Self::NoToken,
        Self::Oversize,
        Self::Malformed,
        Self::Algorithm,
        Self::CriticalHeader,
        Self::MissingKid,
        Self::UnknownKey,
        Self::BadSignature,
        Self::Issuer,
        Self::Audience,
        Self::MissingExp,
        Self::Expired,
        Self::NotYetValid,
        Self::MissingEmail,
        Self::WrongMailbox,
        Self::JwksUnavailable,
        Self::Misconfigured,
    ];

    fn index(self) -> usize {
        Self::ALL.iter().position(|r| *r == self).unwrap_or(0)
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------------------------------------------------------------------------
// JWKS
// ---------------------------------------------------------------------------

/// A monotonic time source. Production reads `Instant::now`; tests step a
/// fake clock past the refetch window instead of sleeping through it.
pub type Clock = Arc<dyn Fn() -> Instant + Send + Sync>;

fn system_clock() -> Clock {
    Arc::new(Instant::now)
}

type KeyMap = HashMap<String, DecodingKey>;

/// After this long, a cached key set is STALE: the next request that needs a
/// key from it refreshes first. Without a maximum age, a key the issuer stops
/// publishing (withdrawn after a leak, say) would stay trusted until some
/// unrelated unknown-`kid` request happened to force a refetch.
pub const JWKS_MAX_AGE: Duration = Duration::from_secs(60 * 60);

/// Hard ceiling: a key set with no successful refresh for this long is not
/// trusted at all (503). A failed refresh keeps serving the old keys so an
/// issuer blip does not log every agent out, but a daemon cut off from its
/// issuer must not trust a key set indefinitely.
pub const JWKS_HARD_CEILING: Duration = Duration::from_secs(24 * 60 * 60);

/// The keys from the last SUCCESSFUL fetch, and when it happened.
#[derive(Default)]
struct KeySet {
    keys: KeyMap,
    /// `None` until a fetch has succeeded.
    fetched_at: Option<Instant>,
}

/// Fetches and caches the issuer's signing keys.
///
/// Lazy: nothing is fetched until a token needs a key, so a daemon whose
/// issuer is briefly unreachable at boot still starts. A fetch happens on an
/// unknown `kid` (rotation) or when a request finds the cached set older than
/// [`JWKS_MAX_AGE`], and never more than once per [`JWKS_MIN_REFETCH`]
/// whatever arrives. A successful fetch REPLACES the set (so a withdrawn key
/// is gone, and a JWKS with no usable keys empties it); a failed one keeps the
/// old set until [`JWKS_HARD_CEILING`].
///
/// Two locks, so that a FRESH CACHE HIT NEVER WAITS ON A FETCH:
/// - `keys` is a plain `RwLock` read on every request and written only for
///   the instant it takes to swap in a freshly fetched set. It is never held
///   across an await.
/// - `refresh` is the async lock a request takes before it may fetch. It is
///   held across the fetch on purpose: concurrent misses queue behind the one
///   fetch and then find its result (or the throttle) instead of each starting
///   their own. A STALE hit only tries it: if a refresh is already in flight,
///   that request is served from the stale set rather than queueing.
pub struct JwksClient {
    url: String,
    http: reqwest::Client,
    min_refetch: Duration,
    now: Clock,
    keys: std::sync::RwLock<Arc<KeySet>>,
    /// When a fetch was last ATTEMPTED, success or not. The throttle keys on
    /// attempts, so a dead endpoint is not hammered either. Only an actual
    /// fetch moves it: a throttled request must not extend the window, or a
    /// steady spray of unknown kids could keep a newly published key locked
    /// out indefinitely.
    refresh: Mutex<Option<Instant>>,
}

enum KeyLookup {
    Found(DecodingKey),
    Unknown,
    Unavailable,
}

impl JwksClient {
    pub fn new(url: impl Into<String>) -> Self {
        Self::with_clock(url, system_clock())
    }

    /// [`JwksClient::new`] with an injected clock. A test seam.
    pub fn with_clock(url: impl Into<String>, now: Clock) -> Self {
        let http = reqwest::Client::builder()
            .timeout(JWKS_TIMEOUT)
            // A redirect is a failed fetch. The https-only check was made on
            // the configured URL; following a `Location` would trust whatever
            // it names, http or anywhere else.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            // Only fails if the TLS backend cannot initialise, which would
            // break every outbound call the daemon makes; a default client
            // still honours nothing worse than no timeout.
            .unwrap_or_default();
        Self {
            url: url.into(),
            http,
            min_refetch: JWKS_MIN_REFETCH,
            now,
            keys: std::sync::RwLock::new(Arc::new(KeySet::default())),
            refresh: Mutex::new(None),
        }
    }

    fn snapshot(&self) -> Arc<KeySet> {
        // A poisoned lock still holds a complete set: the only writer swaps an
        // `Arc` and cannot panic halfway through.
        self.keys.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// How old `set` is at `now`, or `None` if nothing was ever fetched.
    fn age(set: &KeySet, now: Instant) -> Option<Duration> {
        set.fetched_at.map(|t| now.saturating_duration_since(t))
    }

    /// The answer from `set` alone: past the hard ceiling (or never fetched)
    /// nothing is trusted.
    fn lookup(set: &KeySet, kid: &str, now: Instant) -> KeyLookup {
        match Self::age(set, now) {
            Some(age) if age < JWKS_HARD_CEILING => match set.keys.get(kid) {
                Some(k) => KeyLookup::Found(k.clone()),
                None => KeyLookup::Unknown,
            },
            _ => KeyLookup::Unavailable,
        }
    }

    async fn key(&self, kid: &str) -> KeyLookup {
        let set = self.snapshot();
        let now = (self.now)();
        let fresh = Self::age(&set, now).is_some_and(|a| a < JWKS_MAX_AGE);

        // The fresh-hit path: no async lock, so an in-flight refresh never
        // delays it.
        if fresh && let Some(k) = set.keys.get(kid) {
            return KeyLookup::Found(k.clone());
        }

        // A stale hit does not queue behind a refresh someone else started:
        // it is served from the stale set (within the hard ceiling) and that
        // refresh will replace the set for everyone after it.
        let stale_hit = !fresh && set.keys.contains_key(kid);
        let mut last_attempt = if stale_hit {
            match self.refresh.try_lock() {
                Ok(guard) => guard,
                Err(_) => return Self::lookup(&set, kid, now),
            }
        } else {
            self.refresh.lock().await
        };

        // Someone else may have refreshed while this one queued.
        let set = self.snapshot();
        let now = (self.now)();
        let fresh = Self::age(&set, now).is_some_and(|a| a < JWKS_MAX_AGE);
        if fresh && let Some(k) = set.keys.get(kid) {
            return KeyLookup::Found(k.clone());
        }

        let may_fetch =
            last_attempt.is_none_or(|t| now.saturating_duration_since(t) >= self.min_refetch);
        if may_fetch {
            *last_attempt = Some(now);
            match self.fetch().await {
                Ok(keys) => {
                    *self.keys.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(KeySet {
                        keys,
                        fetched_at: Some(now),
                    });
                }
                Err(why) => eprintln!("squelch-mcp: agent door JWKS fetch failed: {why}"),
            }
        }
        drop(last_attempt);

        // Throttled or failed: whatever the current set says, within the
        // ceiling. Succeeded: the new set, and nothing of the old one.
        Self::lookup(&self.snapshot(), kid, (self.now)())
    }

    /// One fetch. The error string is a category, never a body or a URL with
    /// anything secret in it (the JWKS URL is public, but we still do not echo
    /// response content).
    async fn fetch(&self) -> Result<KeyMap, &'static str> {
        let resp = self
            .http
            .get(&self.url)
            .send()
            .await
            .map_err(|_| "request error")?;
        if !resp.status().is_success() {
            return Err("non-success status");
        }
        if resp
            .content_length()
            .is_some_and(|n| n > JWKS_MAX_BYTES as u64)
        {
            return Err("response too large");
        }
        // Read in chunks and stop at the cap: a missing or lying
        // Content-Length must not let the issuer (or anything on the path)
        // make us buffer an unbounded body.
        let mut resp = resp;
        let mut body = Vec::new();
        while let Some(chunk) = resp.chunk().await.map_err(|_| "body read error")? {
            if body.len() + chunk.len() > JWKS_MAX_BYTES {
                return Err("response too large");
            }
            body.extend_from_slice(&chunk);
        }
        let set: JwkSet = serde_json::from_slice(&body).map_err(|_| "not a JWKS")?;
        // An empty result is a SUCCESSFUL answer, not a failure: the issuer
        // has withdrawn every key, and the cache must say so too.
        let keys: KeyMap = set.keys.iter().filter_map(usable_rs256_key).collect();
        Ok(keys)
    }
}

/// The smallest RSA modulus we accept from the JWKS. Below 2048 bits a key is
/// within reach of a well-funded factoring effort, and no issuer we would
/// trust publishes one.
pub const MIN_RSA_BITS: usize = 2048;

/// Bit length of a big-endian unsigned integer given as base64url.
fn modulus_bits(n_b64url: &str) -> Option<usize> {
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(n_b64url.trim_end_matches('='))
        .ok()?;
    let first = bytes.iter().position(|b| *b != 0)?;
    let top = bytes[first];
    Some((bytes.len() - first - 1) * 8 + (8 - top.leading_zeros() as usize))
}

/// Keep only keys that can verify RS256: RSA of at least [`MIN_RSA_BITS`],
/// with a `kid`, not marked for encryption, and not pinned to a different
/// algorithm.
fn usable_rs256_key(jwk: &Jwk) -> Option<(String, DecodingKey)> {
    use jsonwebtoken::jwk::{AlgorithmParameters, KeyAlgorithm, PublicKeyUse};
    let kid = jwk.common.key_id.clone()?;
    let AlgorithmParameters::RSA(rsa) = &jwk.algorithm else {
        return None;
    };
    if modulus_bits(&rsa.n)? < MIN_RSA_BITS {
        return None;
    }
    if matches!(jwk.common.public_key_use, Some(ref u) if *u != PublicKeyUse::Signature) {
        return None;
    }
    if matches!(jwk.common.key_algorithm, Some(a) if a != KeyAlgorithm::RS256) {
        return None;
    }
    let key = DecodingKey::from_jwk(jwk).ok()?;
    Some((kid, key))
}

// ---------------------------------------------------------------------------
// Verifier
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(untagged)]
enum Audience {
    One(String),
    Many(Vec<String>),
}

/// The claims we check. Any of them present with the wrong JSON type makes the
/// whole token Malformed, which is a refusal: fail closed on surprises.
#[derive(Deserialize)]
struct Claims {
    iss: Option<String>,
    aud: Option<Audience>,
    exp: Option<f64>,
    nbf: Option<f64>,
    email: Option<String>,
}

/// A token that passed every check.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Verified {
    /// The token's `exp`, seconds since the epoch.
    pub exp: f64,
}

/// The shortest life a response gets, however close to expiry its token was.
/// A token accepted late in its leeway must still let the call it opened
/// finish: cutting a `tools/call` off mid-flight would return an empty body
/// after the tool had run, instead of a 401 that tells the client to refresh.
pub const RESPONSE_MIN_LIFETIME: Duration = Duration::from_secs(30);

/// The longest life a response gets, whatever the token's `exp` says. A
/// correctly signed token may carry an absurd `exp`; the stream still ends
/// within a day and the client reconnects.
pub const RESPONSE_MAX_LIFETIME: Duration = Duration::from_secs(24 * 60 * 60);

/// How long a response opened by a token with `exp` may keep streaming, at
/// wall-clock time `now` (both seconds since the epoch): its `exp` plus the
/// leeway, floored at [`RESPONSE_MIN_LIFETIME`] and capped at
/// [`RESPONSE_MAX_LIFETIME`]. Total for every f64: no input panics.
pub fn response_lifetime(exp: f64, now: f64) -> Duration {
    let secs = exp + LEEWAY_SECS as f64 - now;
    if secs.is_nan() {
        return RESPONSE_MAX_LIFETIME;
    }
    // NaN, infinite or too big for a Duration: the cap.
    let left = Duration::try_from_secs_f64(secs.max(0.0)).unwrap_or(RESPONSE_MAX_LIFETIME);
    left.clamp(RESPONSE_MIN_LIFETIME, RESPONSE_MAX_LIFETIME)
}

impl Verified {
    /// How long a response this token opened may keep streaming. See
    /// [`response_lifetime`].
    pub fn remaining(&self) -> Duration {
        response_lifetime(self.exp, now_secs())
    }
}

/// Verifies agent-door bearer tokens for ONE mailbox.
pub struct Verifier {
    issuer: String,
    audience: String,
    /// The daemon's account email, trimmed and ASCII-lowercased once.
    account_email: String,
    jwks: JwksClient,
}

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn normalize_email(e: &str) -> String {
    e.trim().to_ascii_lowercase()
}

impl Verifier {
    pub fn new(config: &OAuthConfig, account_email: &str) -> Self {
        Self::with_clock(config, account_email, system_clock())
    }

    /// [`Verifier::new`] with an injected clock for the JWKS throttle. A test
    /// seam; token times (`exp`, `nbf`) still read the wall clock.
    pub fn with_clock(config: &OAuthConfig, account_email: &str, now: Clock) -> Self {
        Self {
            issuer: config.issuer.clone(),
            audience: config.public_url.clone(),
            account_email: normalize_email(account_email),
            jwks: JwksClient::with_clock(config.jwks_url.clone(), now),
        }
    }

    /// Verify an `Authorization` header value (None when absent). `Ok` means
    /// the request may reach the MCP service, and carries the token's expiry,
    /// which bounds how long the response may keep streaming.
    ///
    /// Order matters for cost: everything an unauthenticated caller can make
    /// us do before the signature check is bounded and local. The JWKS is only
    /// consulted for a well-formed RS256 token with a `kid`.
    pub async fn verify(&self, authorization: Option<&str>) -> Result<Verified, Refusal> {
        // Not-a-bearer is "no token" in RFC 6750 terms. This also refuses
        // `sqd_` device tokens and the master token by construction (I3): they
        // are not JWTs, so they fail the parse below, and nothing on this path
        // consults the device-token table or the master token at all.
        let token = authorization
            .and_then(squelch_httpauth::parse_bearer)
            .ok_or(Refusal::NoToken)?;
        if token.len() > MAX_TOKEN_BYTES {
            return Err(Refusal::Oversize);
        }

        // `decode_header` refuses `alg: none` itself (it is not an
        // `Algorithm`), which lands as Malformed; anything else that is not
        // RS256 is refused here, before any key is looked up.
        let header = jsonwebtoken::decode_header(token).map_err(|_| Refusal::Malformed)?;
        if header.alg != Algorithm::RS256 {
            return Err(Refusal::Algorithm);
        }
        // `crit` names header extensions the recipient MUST understand or
        // reject. We understand none, so any `crit` at all is a refusal.
        if header.crit.is_some() {
            return Err(Refusal::CriticalHeader);
        }
        let kid = header.kid.ok_or(Refusal::MissingKid)?;

        let key = match self.jwks.key(&kid).await {
            KeyLookup::Found(k) => k,
            KeyLookup::Unknown => return Err(Refusal::UnknownKey),
            KeyLookup::Unavailable => return Err(Refusal::JwksUnavailable),
        };

        // The library verifies the signature and the algorithm allow-list
        // ONLY. Its claim defaults (aud/iss checked only when present, nbf
        // off) are not what the contract says, so every claim is checked
        // explicitly below instead.
        let mut validation = Validation::new(Algorithm::RS256);
        validation.required_spec_claims.clear();
        validation.validate_exp = false;
        validation.validate_nbf = false;
        validation.validate_aud = false;
        let data = jsonwebtoken::decode::<Claims>(token, &key, &validation).map_err(|e| {
            use jsonwebtoken::errors::ErrorKind;
            match e.kind() {
                ErrorKind::InvalidSignature | ErrorKind::InvalidAlgorithm => Refusal::BadSignature,
                _ => Refusal::Malformed,
            }
        })?;
        let c = data.claims;

        let iss = c.iss.as_deref().ok_or(Refusal::Issuer)?;
        if trim_one_slash(iss) != trim_one_slash(&self.issuer) {
            return Err(Refusal::Issuer);
        }

        let ours = trim_one_slash(&self.audience);
        let aud_ok = match &c.aud {
            Some(Audience::One(a)) => trim_one_slash(a) == ours,
            Some(Audience::Many(v)) => v.iter().any(|a| trim_one_slash(a) == ours),
            None => false,
        };
        if !aud_ok {
            return Err(Refusal::Audience);
        }

        let now = now_secs();
        let leeway = LEEWAY_SECS as f64;
        let exp = c.exp.ok_or(Refusal::MissingExp)?;
        // JSON cannot spell NaN or infinity, but a comparison with NaN is
        // always false, so it would sail past the expiry check: refuse it.
        if !exp.is_finite() {
            return Err(Refusal::Malformed);
        }
        if exp + leeway < now {
            return Err(Refusal::Expired);
        }
        if let Some(nbf) = c.nbf
            && nbf > now + leeway
        {
            return Err(Refusal::NotYetValid);
        }

        // I1, the mailbox binding. See the module docs: without this, any
        // Passband user's token opens every tenant's door.
        let email = c.email.as_deref().ok_or(Refusal::MissingEmail)?;
        if normalize_email(email) != self.account_email || self.account_email.is_empty() {
            return Err(Refusal::WrongMailbox);
        }

        Ok(Verified { exp })
    }
}

// ---------------------------------------------------------------------------
// HTTP
// ---------------------------------------------------------------------------

/// Where refusal summaries go. stderr in production; tests observe it.
pub type LogSink = Arc<dyn Fn(&str) + Send + Sync>;

/// How often at most a refusal summary is written.
pub const REFUSAL_LOG_INTERVAL: Duration = Duration::from_secs(60);

/// Refusal counts per category, written as ONE summary line per interval.
///
/// Refusals are unauthenticated: anyone can produce them as fast as they can
/// send requests, so a line per refusal would hand every stranger a lever on
/// the daemon's log volume. Instead each refusal bumps a counter, and at most
/// one line per [`REFUSAL_LOG_INTERVAL`] reports the non-zero counts. The line
/// carries category names and numbers and nothing else (I6).
pub struct RefusalLog {
    counts: [std::sync::atomic::AtomicU64; Refusal::ALL.len()],
    last_emit: std::sync::Mutex<Instant>,
    interval: Duration,
    now: Clock,
    sink: LogSink,
    ticker_started: std::sync::atomic::AtomicBool,
}

impl RefusalLog {
    /// The production log: stderr, wall clock.
    pub fn stderr() -> Arc<Self> {
        Self::new(
            Arc::new(|line: &str| eprintln!("{line}")),
            system_clock(),
            REFUSAL_LOG_INTERVAL,
        )
    }

    /// A log with an injected sink and clock. A test seam.
    pub fn new(sink: LogSink, now: Clock, interval: Duration) -> Arc<Self> {
        Arc::new(Self {
            counts: std::array::from_fn(|_| std::sync::atomic::AtomicU64::new(0)),
            last_emit: std::sync::Mutex::new(now()),
            interval,
            now,
            sink,
            ticker_started: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// Count one refusal; write a summary if the interval has passed.
    pub fn record(self: &Arc<Self>, why: Refusal) {
        use std::sync::atomic::Ordering;
        self.counts[why.index()].fetch_add(1, Ordering::Relaxed);
        self.flush_if_due();
        self.start_ticker();
    }

    /// Write one summary line if the interval has passed and anything was
    /// counted since the last one.
    pub fn flush_if_due(&self) {
        use std::sync::atomic::Ordering;
        let now = (self.now)();
        let mut last = self.last_emit.lock().unwrap_or_else(|e| e.into_inner());
        if now.saturating_duration_since(*last) < self.interval {
            return;
        }
        *last = now;
        let parts: Vec<String> = Refusal::ALL
            .iter()
            .filter_map(|r| {
                let n = self.counts[r.index()].swap(0, Ordering::Relaxed);
                (n > 0).then(|| format!("{r}={n}"))
            })
            .collect();
        if !parts.is_empty() {
            (self.sink)(&format!(
                "squelch-mcp: agent door refusals since last report: {}",
                parts.join(" ")
            ));
        }
    }

    /// Counts left behind by a burst that then stopped would otherwise wait
    /// for the next refusal. One background task per log, started on first
    /// use (so constructing a router needs no runtime), flushes them; it holds
    /// only a `Weak` and ends when the log is dropped.
    fn start_ticker(self: &Arc<Self>) {
        use std::sync::atomic::Ordering;
        if self.ticker_started.swap(true, Ordering::Relaxed) {
            return;
        }
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let weak = Arc::downgrade(self);
        let interval = self.interval;
        handle.spawn(async move {
            loop {
                tokio::time::sleep(interval).await;
                match weak.upgrade() {
                    Some(log) => log.flush_if_due(),
                    None => return,
                }
            }
        });
    }
}

/// State for [`require_agent_token`].
#[derive(Clone)]
pub struct AuthLayerState {
    verifier: Arc<Verifier>,
    log: Arc<RefusalLog>,
    /// Pre-rendered `WWW-Authenticate` values.
    challenge: HeaderValue,
    challenge_invalid: HeaderValue,
}

impl AuthLayerState {
    /// Crate-private on purpose: the only way to put this guard in front of
    /// an agent door is [`crate::agent_door_router`], which checks the account
    /// email first.
    pub(crate) fn new(config: &OAuthConfig, account_email: &str) -> Self {
        Self::with_parts(
            Verifier::new(config, account_email),
            config,
            RefusalLog::stderr(),
        )
    }

    pub(crate) fn with_parts(
        verifier: Verifier,
        config: &OAuthConfig,
        log: Arc<RefusalLog>,
    ) -> Self {
        let meta = config.resource_metadata_url();
        // The URL came through `url::Url::parse`, so it holds no quote or
        // control character; a header value cannot fail to build from it.
        let challenge = HeaderValue::from_str(&format!("Bearer resource_metadata=\"{meta}\""))
            .unwrap_or_else(|_| HeaderValue::from_static("Bearer"));
        let challenge_invalid = HeaderValue::from_str(&format!(
            "Bearer resource_metadata=\"{meta}\", error=\"invalid_token\""
        ))
        .unwrap_or_else(|_| HeaderValue::from_static("Bearer error=\"invalid_token\""));
        Self {
            verifier: Arc::new(verifier),
            log,
            challenge,
            challenge_invalid,
        }
    }

    /// The response for a refusal: 401 with the right challenge, or 503 when
    /// nothing could ever verify. Empty body either way: nothing in the
    /// response says which check failed (I6).
    fn refusal_response(&self, why: Refusal) -> Response {
        let (status, challenge) = match why {
            Refusal::JwksUnavailable | Refusal::Misconfigured => {
                (StatusCode::SERVICE_UNAVAILABLE, None)
            }
            Refusal::NoToken => (StatusCode::UNAUTHORIZED, Some(self.challenge.clone())),
            _ => (
                StatusCode::UNAUTHORIZED,
                Some(self.challenge_invalid.clone()),
            ),
        };
        let mut resp = Response::new(Body::empty());
        *resp.status_mut() = status;
        if let Some(c) = challenge {
            resp.headers_mut().insert(header::WWW_AUTHENTICATE, c);
        }
        resp
    }
}

/// A response body that ENDS when the token that opened it expires.
///
/// Auth runs once per HTTP request, and an MCP GET opens a server-sent-event
/// stream that can stay up for hours. Without this, a stream opened with a
/// valid token would outlive the token, and the day the server pushes mail
/// down that stream, an expired (or revoked-then-expired) credential would
/// still be receiving it. The client reconnects with a fresh token, which is
/// what refresh tokens are for.
///
/// Chunks pass straight through as they arrive: nothing is buffered. At the
/// deadline the body reports end-of-stream, which closes it cleanly.
struct ExpiringBody {
    inner: Body,
    deadline: std::pin::Pin<Box<tokio::time::Sleep>>,
}

impl http_body::Body for ExpiringBody {
    type Data = axum::body::Bytes;
    type Error = axum::Error;

    fn poll_frame(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
        use std::future::Future as _;
        // Polling the timer also registers this task to be woken at the
        // deadline, so an idle stream is ended on time, not on its next chunk.
        if self.deadline.as_mut().poll(cx).is_ready() {
            return std::task::Poll::Ready(None);
        }
        std::pin::Pin::new(&mut self.inner).poll_frame(cx)
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> http_body::SizeHint {
        self.inner.size_hint()
    }
}

/// axum middleware guarding the agent door in OAuth mode.
pub async fn require_agent_token(
    State(state): State<AuthLayerState>,
    req: Request,
    next: Next,
) -> Response {
    // A non-UTF-8 header is treated as absent: still a refusal.
    let authz = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    match state.verifier.verify(authz).await {
        Ok(verified) => {
            let now = tokio::time::Instant::now();
            // `remaining` is capped at a day, so this cannot overflow in
            // practice; checked anyway, because a panic here resets the
            // client's connection.
            let deadline = now
                .checked_add(verified.remaining())
                .or_else(|| now.checked_add(RESPONSE_MAX_LIFETIME))
                .unwrap_or(now);
            next.run(req).await.map(|inner| {
                Body::new(ExpiringBody {
                    inner,
                    deadline: Box::pin(tokio::time::sleep_until(deadline)),
                })
            })
        }
        Err(why) => {
            // I6: a counted category, summarised at most once per interval.
            state.log.record(why);
            state.refusal_response(why)
        }
    }
}

/// Every `/mcp` request on a misconfigured daemon.
pub async fn misconfigured_door() -> Response {
    let mut resp = Response::new(Body::empty());
    *resp.status_mut() = StatusCode::SERVICE_UNAVAILABLE;
    resp
}

/// The RFC 9728 protected-resource metadata document. Public, unauthenticated.
pub fn protected_resource_metadata(config: &OAuthConfig) -> serde_json::Value {
    serde_json::json!({
        "resource": config.public_url,
        "authorization_servers": [config.issuer],
        "bearer_methods_supported": ["header"],
        "resource_name": "Passband",
    })
}

/// The metadata response, from the document serialized once at startup.
pub(crate) fn metadata_response(body: axum::body::Bytes) -> Response {
    let mut resp = Response::new(Body::from(body));
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    // Browser-based MCP clients fetch this cross-origin; it is public.
    h.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=300"),
    );
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neither_set_is_open_and_blank_is_unset() {
        assert_eq!(
            AgentDoorAuth::from_values(None, None, None),
            AgentDoorAuth::Open
        );
        assert_eq!(
            AgentDoorAuth::from_values(Some("  "), Some(""), Some("https://x/jwks")),
            AgentDoorAuth::Open
        );
    }

    #[test]
    fn exactly_one_set_is_misconfigured_never_open() {
        assert_eq!(
            AgentDoorAuth::from_values(Some("https://a.authkit.app"), None, None),
            AgentDoorAuth::Misconfigured(MisconfiguredReason::IssuerWithoutPublicUrl)
        );
        assert_eq!(
            AgentDoorAuth::from_values(None, Some("https://alice.passband.email/mcp"), None),
            AgentDoorAuth::Misconfigured(MisconfiguredReason::PublicUrlWithoutIssuer)
        );
    }

    #[test]
    fn unparseable_or_plain_http_values_are_misconfigured() {
        let p = Some("https://alice.passband.email/mcp");
        for bad in [
            "not a url",
            "http://a.authkit.app",
            "https://u:p@a.authkit.app",
        ] {
            assert_eq!(
                AgentDoorAuth::from_values(Some(bad), p, None),
                AgentDoorAuth::Misconfigured(MisconfiguredReason::InvalidIssuer),
                "{bad}"
            );
        }
        assert_eq!(
            AgentDoorAuth::from_values(Some("https://a.authkit.app"), Some("alice/mcp"), None),
            AgentDoorAuth::Misconfigured(MisconfiguredReason::InvalidPublicUrl)
        );
        assert_eq!(
            AgentDoorAuth::from_values(Some("https://a.authkit.app"), p, Some("ftp://x")),
            AgentDoorAuth::Misconfigured(MisconfiguredReason::InvalidJwksUrl)
        );
    }

    #[test]
    fn oauth_mode_derives_jwks_metadata_url_and_public_hosts() {
        let AgentDoorAuth::OAuth(c) = AgentDoorAuth::from_values(
            Some(" https://a.authkit.app/ "),
            Some("https://alice.passband.email/mcp"),
            None,
        ) else {
            panic!("expected OAuth");
        };
        assert_eq!(c.jwks_url(), "https://a.authkit.app/oauth2/jwks");
        assert_eq!(
            c.resource_metadata_url(),
            "https://alice.passband.email/.well-known/oauth-protected-resource/mcp"
        );
        assert_eq!(c.public_hosts(), ["alice.passband.email"]);

        let AgentDoorAuth::OAuth(c) = AgentDoorAuth::from_values(
            Some("http://127.0.0.1:9/"),
            Some("http://localhost:8848/mcp"),
            Some("http://127.0.0.1:9/keys"),
        ) else {
            panic!("expected OAuth for loopback http");
        };
        assert_eq!(c.jwks_url(), "http://127.0.0.1:9/keys");
        assert_eq!(c.public_hosts(), ["localhost", "localhost:8848"]);
    }

    /// A clock the test moves by hand, and a sink that keeps every line.
    fn fake_log(
        interval: Duration,
    ) -> (
        Arc<RefusalLog>,
        Arc<std::sync::Mutex<Vec<String>>>,
        Arc<std::sync::atomic::AtomicU64>,
    ) {
        use std::sync::atomic::{AtomicU64, Ordering};
        let lines = Arc::new(std::sync::Mutex::new(Vec::new()));
        let offset = Arc::new(AtomicU64::new(0));
        let base = Instant::now();
        let clock: Clock = {
            let offset = offset.clone();
            Arc::new(move || base + Duration::from_secs(offset.load(Ordering::SeqCst)))
        };
        let sink: LogSink = {
            let lines = lines.clone();
            Arc::new(move |l: &str| lines.lock().unwrap().push(l.to_string()))
        };
        (RefusalLog::new(sink, clock, interval), lines, offset)
    }

    #[test]
    fn refusals_are_summarised_at_most_once_per_interval() {
        use std::sync::atomic::Ordering;
        let (log, lines, clock) = fake_log(Duration::from_secs(60));
        for _ in 0..2000 {
            log.record(Refusal::NoToken);
        }
        log.record(Refusal::Expired);
        assert!(
            lines.lock().unwrap().is_empty(),
            "nothing inside the interval"
        );

        clock.store(61, Ordering::SeqCst);
        log.record(Refusal::NoToken);
        log.record(Refusal::NoToken);
        let got = lines.lock().unwrap().clone();
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(
            got[0],
            "squelch-mcp: agent door refusals since last report: no_token=2001 expired=1"
        );

        // The counters reset; a quiet interval writes nothing.
        clock.store(200, Ordering::SeqCst);
        log.flush_if_due();
        assert_eq!(
            lines.lock().unwrap().len(),
            2,
            "the leftover one is reported"
        );
        clock.store(300, Ordering::SeqCst);
        log.flush_if_due();
        assert_eq!(lines.lock().unwrap().len(), 2, "silence writes nothing");
    }

    /// Through the real middleware: a flood of anonymous requests writes a
    /// bounded number of lines, not one per request.
    #[tokio::test]
    async fn a_flood_of_refused_requests_does_not_flood_the_log() {
        use tower::ServiceExt as _;
        let AgentDoorAuth::OAuth(config) = AgentDoorAuth::from_values(
            Some("https://a.authkit.app"),
            Some("https://alice.passband.email/mcp"),
            Some("http://127.0.0.1:9/jwks"),
        ) else {
            panic!("expected OAuth");
        };
        let (log, lines, _clock) = fake_log(Duration::from_secs(60));
        let state =
            AuthLayerState::with_parts(Verifier::new(&config, "alice@example.com"), &config, log);
        let app = axum::Router::new()
            .route("/mcp", axum::routing::any(|| async { "reached" }))
            .route_layer(axum::middleware::from_fn_with_state(
                state,
                require_agent_token,
            ));
        for i in 0..500 {
            let req = axum::http::Request::builder()
                .method("POST")
                .uri("/mcp")
                .header(
                    "authorization",
                    if i % 2 == 0 { "Bearer junk" } else { "Basic x" },
                )
                .body(Body::empty())
                .unwrap();
            let resp = app.clone().oneshot(req).await.unwrap();
            assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        }
        assert!(
            lines.lock().unwrap().len() <= 1,
            "{:?}",
            lines.lock().unwrap()
        );
    }

    #[test]
    fn modulus_bits_counts_from_the_top_set_bit() {
        use base64::Engine as _;
        let enc = |b: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b);
        assert_eq!(modulus_bits(&enc(&[0x01])), Some(1));
        assert_eq!(modulus_bits(&enc(&[0x00, 0x80, 0x00])), Some(16));
        assert_eq!(modulus_bits(&enc(&[0xff; 256])), Some(2048));
        assert_eq!(modulus_bits(&enc(&[0x7f; 256])), Some(2047));
        assert_eq!(modulus_bits(&enc(&[0x00])), None);
    }

    #[test]
    fn a_response_lifetime_is_floored_capped_and_never_panics() {
        let now = 1_000_000.0;
        // Plenty left: exp + leeway - now.
        assert_eq!(
            response_lifetime(now + 600.0, now),
            Duration::from_secs(660)
        );
        // Accepted late in its leeway: still the floor.
        assert_eq!(response_lifetime(now - 59.5, now), RESPONSE_MIN_LIFETIME);
        assert_eq!(response_lifetime(now - 1000.0, now), RESPONSE_MIN_LIFETIME);
        // Absurd or non-finite: the cap, no panic.
        for exp in [1e19, 1e300, f64::MAX, f64::INFINITY, f64::NAN] {
            assert_eq!(response_lifetime(exp, now), RESPONSE_MAX_LIFETIME, "{exp}");
        }
        assert_eq!(
            response_lifetime(f64::NEG_INFINITY, now),
            RESPONSE_MIN_LIFETIME
        );
    }

    /// A token accepted with about a second of leeway left still gets the
    /// whole response from a handler that takes longer than that.
    #[tokio::test]
    async fn a_slow_response_to_a_late_token_is_not_cut_off() {
        use crate::testing::{KEY_A_PEM, KID_A, StubJwks, sign, unix_now, valid_claims};
        use tower::ServiceExt as _;
        let jwks = StubJwks::start().await;
        let AgentDoorAuth::OAuth(config) = AgentDoorAuth::from_values(
            Some("https://a.authkit.app"),
            Some("https://alice.passband.email/mcp"),
            Some(&jwks.url),
        ) else {
            panic!("expected OAuth");
        };
        let (log, _lines, _clock) = fake_log(Duration::from_secs(60));
        let state =
            AuthLayerState::with_parts(Verifier::new(&config, "alice@example.com"), &config, log);
        let app = axum::Router::new()
            .route(
                "/mcp",
                axum::routing::any(|| async {
                    tokio::time::sleep(Duration::from_secs(3)).await;
                    "the whole answer"
                }),
            )
            .route_layer(axum::middleware::from_fn_with_state(
                state,
                require_agent_token,
            ));
        let mut c = valid_claims(
            "https://a.authkit.app",
            "https://alice.passband.email/mcp",
            "alice@example.com",
        );
        c["exp"] = serde_json::json!(unix_now() - 59);
        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/mcp")
            .header(
                "authorization",
                format!("Bearer {}", sign(&c, KEY_A_PEM, KID_A)),
            )
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(&body[..], b"the whole answer");
    }
}
