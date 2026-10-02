//! Test support for the agent door's OAuth mode: two throwaway RSA keys, a
//! token signer, and an in-process JWKS server that counts its fetches.
//!
//! Compiled only with the `testing` feature, which nothing but dev-dependencies
//! enable, so the fixture keys never reach a shipped binary. Shared here so
//! squelchd's router tests drive the same keys as this crate's own, rather
//! than a second copy of them.
//!
//! The keys under `tests/fixtures/` were generated for these tests and are
//! used nowhere else. They sign nothing real.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::http::header;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde_json::{Value, json};

/// Private key A: the issuer's live signing key in most tests.
pub const KEY_A_PEM: &str = include_str!("../tests/fixtures/test-key-a.pem");
/// Key A's public half, for the HS256 algorithm-confusion test.
pub const KEY_A_PUB_PEM: &str = include_str!("../tests/fixtures/test-key-a.pub.pem");
/// Private key B: "some other key" (a forger's, or a newly rotated one).
pub const KEY_B_PEM: &str = include_str!("../tests/fixtures/test-key-b.pem");
/// Key A's modulus, base64url, exactly as a JWKS publishes it.
pub const KEY_A_N: &str = "4ySymk0-_nx55n5FqlLuimIkA4LNDUKlzLCkUhUspEoDm3uuBEOiGC0FNyiXhN6DO9Y4A71spGM1tZhTGhAUNWZ9sNp_k4kEeqbIUqS6mf4hdwtFkFTEslL1qmBBMoszMkMoEVx8SDNEsEGKs-M1eKZrSGsF8a_Oen0f8knWl0vnszeSErUFqYdjPTkR26HFTSdAubok0iYPiOKL0wdmHsD0WU8smEO_BcjAmUfELaEWQ6tCZXxWiBtwr1Ylf_dDUcmGxxymJ3Bt_CxBQNrYnAaOodsbkoEz8Do1er3eOCHXH4Arjie9u6Xrf0FBstFTX_ZqlEJzMId4cdoFndlmdw";
/// Key B's modulus, base64url.
pub const KEY_B_N: &str = "0aa-HER3FHGb3bUNEf_ZQnQRffWCXShLoW02mEBUvX12JJ54he2Tc-TXRfBMRTlzbikUteuYZFVLIoK92uOSrNZhEyaFj61AEpHjTAjtyzmKqspoPy5pB_-7FYWhKP0tW72FYSXYuPFsmReee0LMAziL9ab9oNJXibQrD8e55kzRuxGBJ6U3IXimtE7RNOoozA6SUUfIMlWVRxNiYh4k4sQ9lcxzVls-jkm0pJ1YDS2n2MaiKhlmtX4yvz_JKYoNyVpSpPtkwDEqETWZisEUBk7Wh3KJr1yvw3NW5FNdR2vv8EXUMPTMcJFwB9XGSxEM-kaZ7-ErP2LnEQ3TdrbFUw";
/// The `kid` key A is published under.
pub const KID_A: &str = "test-kid-1";
/// The `kid` key B is published under once it is rotated in.
pub const KID_B: &str = "test-kid-2";

/// One RS256 signing JWK for a JWKS document.
pub fn jwk(kid: &str, n: &str) -> Value {
    json!({ "kty": "RSA", "kid": kid, "use": "sig", "alg": "RS256", "n": n, "e": "AQAB" })
}

/// Seconds since the epoch, for building `exp` / `nbf` / `iat`.
pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Claims that pass every check for a daemon configured with this issuer,
/// public URL and account. Tests mutate one field at a time.
pub fn valid_claims(issuer: &str, public_url: &str, email: &str) -> Value {
    json!({
        "iss": issuer,
        "aud": public_url,
        "sub": email,
        "email": email,
        "iat": unix_now(),
        "exp": unix_now() + 600,
    })
}

/// Sign `claims` RS256 with a private key PEM under `kid`.
pub fn sign(claims: &Value, pem: &str, kid: &str) -> String {
    let mut h = Header::new(Algorithm::RS256);
    h.kid = Some(kid.to_string());
    let key = EncodingKey::from_rsa_pem(pem.as_bytes()).expect("fixture key parses");
    jsonwebtoken::encode(&h, claims, &key).expect("signs")
}

/// An in-process JWKS endpoint on `127.0.0.1:0`, serving `/oauth2/jwks`.
///
/// It counts every request, can change what it publishes (key rotation), and
/// can be told to stall before answering (a slow issuer).
pub struct StubJwks {
    /// The full JWKS URL to configure the daemon with.
    pub url: String,
    fetches: Arc<AtomicUsize>,
    doc: Arc<RwLock<Value>>,
    delay_ms: Arc<AtomicU64>,
}

impl StubJwks {
    /// Start a stub publishing key A under [`KID_A`].
    pub async fn start() -> Self {
        Self::start_with(json!({ "keys": [jwk(KID_A, KEY_A_N)] })).await
    }

    /// Start a stub publishing `doc` verbatim.
    pub async fn start_with(doc: Value) -> Self {
        let fetches = Arc::new(AtomicUsize::new(0));
        let doc = Arc::new(RwLock::new(doc));
        let delay_ms = Arc::new(AtomicU64::new(0));
        let app = {
            let (fetches, doc, delay_ms) = (fetches.clone(), doc.clone(), delay_ms.clone());
            axum::Router::new().route(
                "/oauth2/jwks",
                axum::routing::get(move || {
                    let (fetches, doc, delay_ms) = (fetches.clone(), doc.clone(), delay_ms.clone());
                    async move {
                        fetches.fetch_add(1, Ordering::SeqCst);
                        let delay = delay_ms.load(Ordering::SeqCst);
                        if delay > 0 {
                            tokio::time::sleep(Duration::from_millis(delay)).await;
                        }
                        let body = doc.read().expect("doc lock").to_string();
                        ([(header::CONTENT_TYPE, "application/json")], body)
                    }
                }),
            )
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind stub JWKS");
        let addr = listener.local_addr().expect("stub addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Self {
            url: format!("http://{addr}/oauth2/jwks"),
            fetches,
            doc,
            delay_ms,
        }
    }

    /// How many requests the stub has received.
    pub fn fetches(&self) -> usize {
        self.fetches.load(Ordering::SeqCst)
    }

    /// Replace the published document (key rotation).
    pub fn publish(&self, doc: Value) {
        *self.doc.write().expect("doc lock") = doc;
    }

    /// Stall every later answer by `delay` (counted on arrival).
    pub fn set_delay(&self, delay: Duration) {
        self.delay_ms
            .store(delay.as_millis() as u64, Ordering::SeqCst);
    }
}
