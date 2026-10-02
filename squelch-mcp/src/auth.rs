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
        }
    }
}

impl std::fmt::Display for MisconfiguredReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
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

/// Fetches and caches the issuer's signing keys.
///
/// Lazy: nothing is fetched until a token needs a key, so a daemon whose
/// issuer is briefly unreachable at boot still starts. Refetches on an unknown
/// `kid` (key rotation), but never more than once per [`JWKS_MIN_REFETCH`].
///
/// Two locks, so that a CACHE HIT NEVER WAITS ON A FETCH:
/// - `keys` is a plain `RwLock` read on every request and written only for
///   the instant it takes to swap in a freshly fetched map. It is never held
///   across an await.
/// - `refresh` is the async lock a MISS takes before it may fetch. It is held
///   across the fetch on purpose: concurrent misses queue behind the one fetch
///   and then find its result (or the throttle) instead of each starting their
///   own.
pub struct JwksClient {
    url: String,
    http: reqwest::Client,
    min_refetch: Duration,
    now: Clock,
    /// RS256-usable keys by `kid`. Replaced wholesale on a successful fetch,
    /// kept as-is on a failed one (a blip at the issuer must not log every
    /// agent out).
    keys: std::sync::RwLock<Arc<KeyMap>>,
    /// When a fetch was last ATTEMPTED, success or not. The throttle keys on
    /// attempts, so a dead endpoint is not hammered either. Only an actual
    /// fetch moves it: a throttled miss must not extend the window, or a
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
            keys: std::sync::RwLock::new(Arc::new(KeyMap::new())),
            refresh: Mutex::new(None),
        }
    }

    fn snapshot(&self) -> Arc<KeyMap> {
        // A poisoned lock still holds a complete map: the only writer swaps an
        // `Arc` and cannot panic halfway through.
        self.keys.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    async fn key(&self, kid: &str) -> KeyLookup {
        // The hit path: no async lock, so an in-flight refresh never delays it.
        if let Some(k) = self.snapshot().get(kid) {
            return KeyLookup::Found(k.clone());
        }

        let mut last_attempt = self.refresh.lock().await;
        // Another miss may have refreshed while this one queued.
        if let Some(k) = self.snapshot().get(kid) {
            return KeyLookup::Found(k.clone());
        }
        let now = (self.now)();
        let may_fetch =
            last_attempt.is_none_or(|t| now.saturating_duration_since(t) >= self.min_refetch);
        if may_fetch {
            *last_attempt = Some(now);
            match self.fetch().await {
                Ok(keys) => {
                    *self.keys.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(keys);
                }
                Err(why) => eprintln!("squelch-mcp: agent door JWKS fetch failed: {why}"),
            }
        }
        drop(last_attempt);

        let keys = self.snapshot();
        match keys.get(kid) {
            Some(k) => KeyLookup::Found(k.clone()),
            None if keys.is_empty() => KeyLookup::Unavailable,
            None => KeyLookup::Unknown,
        }
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
        let body = resp.bytes().await.map_err(|_| "body read error")?;
        if body.len() > JWKS_MAX_BYTES {
            return Err("response too large");
        }
        let set: JwkSet = serde_json::from_slice(&body).map_err(|_| "not a JWKS")?;
        let keys: KeyMap = set.keys.iter().filter_map(usable_rs256_key).collect();
        if keys.is_empty() {
            return Err("no usable RS256 keys");
        }
        Ok(keys)
    }
}

/// Keep only keys that can verify RS256: RSA, with a `kid`, not marked for
/// encryption, and not pinned to a different algorithm.
fn usable_rs256_key(jwk: &Jwk) -> Option<(String, DecodingKey)> {
    use jsonwebtoken::jwk::{AlgorithmParameters, KeyAlgorithm, PublicKeyUse};
    let kid = jwk.common.key_id.clone()?;
    if !matches!(jwk.algorithm, AlgorithmParameters::RSA(_)) {
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

    /// Verify an `Authorization` header value (None when absent). `Ok(())`
    /// means the request may reach the MCP service.
    ///
    /// Order matters for cost: everything an unauthenticated caller can make
    /// us do before the signature check is bounded and local. The JWKS is only
    /// consulted for a well-formed RS256 token with a `kid`.
    pub async fn verify(&self, authorization: Option<&str>) -> Result<(), Refusal> {
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

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// HTTP
// ---------------------------------------------------------------------------

/// State for [`require_agent_token`].
#[derive(Clone)]
pub struct AuthLayerState {
    verifier: Arc<Verifier>,
    /// Pre-rendered `WWW-Authenticate` values.
    challenge: HeaderValue,
    challenge_invalid: HeaderValue,
}

impl AuthLayerState {
    pub fn new(config: &OAuthConfig, account_email: &str) -> Self {
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
            verifier: Arc::new(Verifier::new(config, account_email)),
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
        Ok(()) => next.run(req).await,
        Err(why) => {
            // I6: the category only.
            eprintln!("squelch-mcp: agent door refused a request: {why}");
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
}
