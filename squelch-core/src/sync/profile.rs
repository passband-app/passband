//! The account's Google profile: display name and profile photo.
//!
//! Gmail's API knows an address and nothing else about its owner, so this is
//! the one call the sync loop makes outside it: `people.get(people/me)` on the
//! People API, which answers only when the grant carries the optional
//! [`GOOGLE_PROFILE_SCOPE`](crate::config::GOOGLE_PROFILE_SCOPE).
//!
//! EVERYTHING HERE IS COSMETIC, and the code is shaped around that. A grant
//! without the scope (every credential minted before it was requested, and
//! anyone who unticked it) answers 403 forever; that is the normal case, said
//! once per process and then retried quietly, because the day somebody re-runs
//! `squelchd auth` it starts working with no restart. Nothing in this file can
//! fail a sync cycle, touch the Gmail quota budget, or move a Gmail metric.
//!
//! The photo is fetched ONCE HERE and served from the store. Clients never see
//! Google's URL: it would make every app launch a request to Google that names
//! the person, and the human door is supposed to be the only thing a client
//! talks to.

use std::sync::atomic::Ordering;
use std::time::Duration;

use chrono::Utc;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::SyncEngine;
use crate::credentials::CredentialStore;
use crate::store::{AccountPhoto, Store};

/// `people.get` for the signed-in person, asking for the two fields we store.
pub(super) const PEOPLE_ME_URL: &str =
    "https://people.googleapis.com/v1/people/me?personFields=names,photos";

/// A day between refreshes once one has worked: a profile photo changes a few
/// times a decade.
const REFRESH_EVERY_SECS: i64 = 24 * 60 * 60;

/// An hour between attempts while it is NOT working. Short enough that a
/// re-auth which adds the scope shows a face the same afternoon, long enough
/// that a grant which will never carry it costs 24 small requests a day.
const RETRY_EVERY_SECS: i64 = 60 * 60;

/// One round trip's budget. This runs on the poll lane ahead of the walk, so a
/// wedged connection here is mail that is late.
const PROFILE_HTTP_TIMEOUT: Duration = Duration::from_secs(15);

/// The People answer is a few hundred bytes of JSON.
const MAX_PERSON_BYTES: usize = 64 * 1024;

/// The edge we ask Google's image server for. Twice the largest size any
/// client draws it at, so it stays sharp on a 2x display.
const PHOTO_EDGE_PX: u32 = 256;

/// A 256px JPEG is tens of kilobytes. The cap is what keeps a wrong URL or a
/// hostile answer from becoming a megabyte blob on every stats-adjacent read.
const MAX_PHOTO_BYTES: usize = 1024 * 1024;

#[derive(Debug, Deserialize)]
struct PersonResp {
    #[serde(default)]
    names: Vec<PersonName>,
    #[serde(default)]
    photos: Vec<PersonPhoto>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PersonName {
    #[serde(default)]
    metadata: FieldMeta,
    display_name: Option<String>,
    given_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PersonPhoto {
    #[serde(default)]
    metadata: FieldMeta,
    url: Option<String>,
    /// Google's own flag for "this is the generated letter-on-a-disc, not a
    /// picture the person chose".
    #[serde(default)]
    default: bool,
}

#[derive(Debug, Default, Deserialize)]
struct FieldMeta {
    #[serde(default)]
    primary: bool,
}

/// What we keep from a People answer.
#[derive(Debug, PartialEq, Eq)]
struct Person {
    name: Option<String>,
    given_name: Option<String>,
    photo_url: Option<String>,
}

/// Trimmed, and `None` when nothing is left: an empty name is not a name.
fn clean(s: Option<String>) -> Option<String> {
    let s = s?.trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// Pick the primary name and the primary photo, falling back to the first of
/// each (an account with one name does not always mark it primary).
///
/// A DEFAULT PHOTO IS NO PHOTO. Google generates a colored disc with a letter
/// on it for accounts that never set a picture, and the client already draws
/// its own initial in its own colors; importing Google's would replace a mark
/// that matches the app with one that does not, and call it a feature.
fn pick(resp: PersonResp) -> Person {
    let name_at = resp
        .names
        .iter()
        .position(|n| n.metadata.primary)
        .unwrap_or(0);
    let (name, given_name) = match resp.names.into_iter().nth(name_at) {
        Some(n) => (clean(n.display_name), clean(n.given_name)),
        None => (None, None),
    };
    let photo_at = resp
        .photos
        .iter()
        .position(|p| p.metadata.primary)
        .unwrap_or(0);
    let photo_url = resp
        .photos
        .into_iter()
        .nth(photo_at)
        .filter(|p| !p.default)
        .and_then(|p| clean(p.url));
    Person {
        name,
        given_name,
        photo_url,
    }
}

/// The URL to actually download: Google's, re-sized to a square
/// [`PHOTO_EDGE_PX`] crop. `None` for anything that is not an https URL on
/// Google's image host.
///
/// The host check is not about trusting the People API less than we do; it is
/// that this is the one place the daemon fetches a URL it was HANDED, from
/// inside a network where other things listen. A URL that is not where Google
/// keeps profile photos is not fetched.
///
/// Sizing: these URLs end in an options suffix after the last `=` ("=s100"),
/// and the path itself never contains one, so the suffix is replaced when
/// present and appended when not.
fn photo_fetch_url(raw: &str, allow_loopback: bool) -> Option<url::Url> {
    let parsed = url::Url::parse(raw).ok()?;
    let host = parsed.host_str()?;
    let google = parsed.scheme() == "https"
        && (host == "googleusercontent.com" || host.ends_with(".googleusercontent.com"));
    let loopback = allow_loopback && host == "127.0.0.1";
    if !(google || loopback) || parsed.query().is_some() {
        return None;
    }
    let base = match raw.rsplit_once('=') {
        Some((base, _options)) => base,
        None => raw,
    };
    url::Url::parse(&format!("{base}=s{PHOTO_EDGE_PX}-c")).ok()
}

/// The image type, read off the bytes. A `Content-Type` header is a claim; this
/// is what a client's decoder will actually be handed, and anything that is not
/// one of these four is not stored.
fn sniff_image(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        Some("image/png")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

/// Sixteen hex characters of SHA-256: plenty to tell two photos apart, short
/// enough to sit in a stats answer and an ETag.
fn photo_tag(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Google's machine-readable reasons for a refusal (`error.details[].reason`),
/// which is where it says WHY a 403 is a 403. Each is an enum constant; only
/// that shape is kept, so nothing from a response body reaches a log line
/// except capitals and underscores.
fn refusal_reasons(body: &[u8]) -> Vec<String> {
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(body) else {
        return Vec::new();
    };
    v["error"]["details"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|d| d["reason"].as_str())
        .filter(|r| {
            !r.is_empty() && r.len() <= 64 && r.bytes().all(|b| b.is_ascii_uppercase() || b == b'_')
        })
        .map(str::to_string)
        .collect()
}

/// What to tell the operator about a 401/403 from the People API. The two
/// common causes look identical by status and are fixed in different places:
/// one on Google's consent screen, one in the Cloud console.
fn refusal_message(status: u16, body: &[u8]) -> String {
    let reasons = refusal_reasons(body);
    let has = |want: &str| reasons.iter().any(|r| r == want);
    if has("SERVICE_DISABLED") {
        "the People API is not enabled on the OAuth client's Google Cloud project; enable it \
         under APIs & Services, then restart the daemon"
            .to_string()
    } else if has("ACCESS_TOKEN_SCOPE_INSUFFICIENT") {
        "the Google grant does not include name and photo; re-run `squelchd auth` with this \
         build, or reconnect the mailbox, to add them"
            .to_string()
    } else if reasons.is_empty() {
        format!("the People API refused with {status} and gave no reason")
    } else {
        format!(
            "the People API refused with {status}: {}",
            reasons.join(", ")
        )
    }
}

/// Read a body up to `cap` bytes, refusing one that runs past it rather than
/// truncating: half a JPEG is not a smaller JPEG.
async fn read_capped(
    mut resp: reqwest::Response,
    cap: usize,
    what: &str,
) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    loop {
        match resp.chunk().await {
            Ok(Some(chunk)) => {
                if out.len() + chunk.len() > cap {
                    return Err(format!("{what} ran past {cap} bytes"));
                }
                out.extend_from_slice(&chunk);
            }
            Ok(None) => return Ok(out),
            Err(e) => return Err(format!("reading {what} failed ({e})")),
        }
    }
}

impl<S: Store + 'static, C: CredentialStore + 'static + ?Sized> SyncEngine<S, C> {
    /// Refresh the stored name and photo if one is due. Called every poll tick;
    /// almost every call is one atomic load and a return.
    ///
    /// Cannot fail a cycle, like the unread count beside it. On any failure the
    /// previous row stands and the next attempt is an hour out.
    pub(super) async fn refresh_profile(&self) {
        let now = Utc::now().timestamp();
        if now < self.profile_due.load(Ordering::Relaxed) {
            return;
        }
        let (next, outcome) = match self.fetch_profile().await {
            Ok(()) => (REFRESH_EVERY_SECS, Ok(())),
            Err(why) => (RETRY_EVERY_SECS, Err(why)),
        };
        self.profile_due.store(now + next, Ordering::Relaxed);
        match outcome {
            Ok(()) => self.profile_warned.store(false, Ordering::Relaxed),
            // Said on the transition into failure only. For a grant without the
            // scope that is once per process, which is the right volume for a
            // missing picture.
            Err(why) => {
                if !self.profile_warned.swap(true, Ordering::Relaxed) {
                    eprintln!("squelch: no Google name or photo for this account ({why})");
                }
            }
        }
    }

    /// One whole refresh: ask who this is, download the photo if there is one,
    /// and only then write. Nothing is stored unless every step worked, so a
    /// photo that failed to download never blanks the one already held.
    ///
    /// Errors are plain strings built here: they carry statuses and sizes,
    /// never a token, a URL's options, or a response body.
    async fn fetch_profile(&self) -> Result<(), String> {
        // Straight off the credential store rather than through the Gmail
        // token path, which counts failures against Gmail's own metrics and
        // sets the "mailbox disconnected" state. A profile that will not load
        // is not a disconnected mailbox.
        let token = self
            .creds
            .token(self.account_id)
            .await
            .map_err(|_| "the credential is not usable right now".to_string())?;

        let resp = self
            .http
            .get(&self.profile_url)
            .bearer_auth(&token.access_token)
            .timeout(PROFILE_HTTP_TIMEOUT)
            .send()
            .await
            .map_err(|e| format!("the People API did not answer ({})", e.without_url()))?;
        let status = resp.status().as_u16();
        match status {
            200 => {}
            // Two different problems share these statuses, and the fix for
            // each is in a different place, so Google's own reason is read
            // rather than guessed at.
            401 | 403 => {
                let body = read_capped(resp, MAX_PERSON_BYTES, "the People API refusal")
                    .await
                    .unwrap_or_default();
                return Err(refusal_message(status, &body));
            }
            other => return Err(format!("the People API answered {other}")),
        }
        let body = read_capped(resp, MAX_PERSON_BYTES, "the People API answer").await?;
        let person = pick(
            serde_json::from_slice::<PersonResp>(&body)
                .map_err(|_| "the People API answer was not a person".to_string())?,
        );

        let photo = match &person.photo_url {
            Some(raw) => Some(self.download_photo(raw).await?),
            None => None,
        };
        self.store
            .set_account_profile(
                self.account_id,
                person.name.as_deref(),
                person.given_name.as_deref(),
                photo.as_ref(),
            )
            .map_err(|e| format!("storing the profile failed ({e})"))
    }

    /// Download one profile photo. NO BEARER TOKEN on this request: the URL is
    /// public by construction, and a credential has no business travelling to
    /// an image host.
    async fn download_photo(&self, raw: &str) -> Result<AccountPhoto, String> {
        let url = photo_fetch_url(raw, cfg!(test))
            .ok_or_else(|| "the photo is not on Google's image host".to_string())?;
        let resp = self
            .http
            .get(url)
            .timeout(PROFILE_HTTP_TIMEOUT)
            .send()
            .await
            .map_err(|e| format!("the photo did not download ({})", e.without_url()))?;
        if !resp.status().is_success() {
            return Err(format!(
                "the photo download answered {}",
                resp.status().as_u16()
            ));
        }
        let bytes = read_capped(resp, MAX_PHOTO_BYTES, "the photo").await?;
        let mime = sniff_image(&bytes).ok_or_else(|| "the photo is not an image".to_string())?;
        Ok(AccountPhoto {
            tag: photo_tag(&bytes),
            mime: mime.to_string(),
            bytes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::error::Result as CoreResult;
    use crate::store::sqlite::SqliteStore;
    use crate::types::AccountId;
    use async_trait::async_trait;
    use axum::extract::State;
    use axum::http::StatusCode;
    use axum::response::{IntoResponse, Response};
    use axum::routing::get;
    use axum::{Json, Router};
    use serde_json::{Value, json};
    use std::sync::{Arc, Mutex};

    const JPEG: &[u8] = &[0xFF, 0xD8, 0xFF, 0xE0, 0, 0x10, b'J', b'F', b'I', b'F'];

    fn person(v: Value) -> Person {
        pick(serde_json::from_value(v).unwrap())
    }

    #[test]
    fn picks_the_primary_name_and_photo() {
        let got = person(json!({
            "names": [
                {"metadata": {}, "displayName": "A. Lovelace", "givenName": "A."},
                {"metadata": {"primary": true}, "displayName": " Ada Lovelace ", "givenName": "Ada"},
            ],
            "photos": [
                {"metadata": {}, "url": "https://lh3.googleusercontent.com/old=s100"},
                {"metadata": {"primary": true}, "url": "https://lh3.googleusercontent.com/new=s100"},
            ],
        }));
        assert_eq!(got.name.as_deref(), Some("Ada Lovelace"));
        assert_eq!(got.given_name.as_deref(), Some("Ada"));
        assert_eq!(
            got.photo_url.as_deref(),
            Some("https://lh3.googleusercontent.com/new=s100")
        );
    }

    #[test]
    fn a_generated_default_photo_is_no_photo() {
        let got = person(json!({
            "names": [{"displayName": "Ada Lovelace"}],
            "photos": [{"url": "https://lh3.googleusercontent.com/a/x=s100", "default": true}],
        }));
        assert_eq!(got.name.as_deref(), Some("Ada Lovelace"));
        assert_eq!(got.photo_url, None);
        // And a person with nothing at all is still a person.
        assert_eq!(
            person(json!({})),
            Person {
                name: None,
                given_name: None,
                photo_url: None
            }
        );
        assert_eq!(person(json!({"names": [{"displayName": "  "}]})).name, None);
    }

    #[test]
    fn only_googles_image_host_is_fetched_and_always_resized() {
        let sized = |raw| photo_fetch_url(raw, false).map(|u| u.to_string());
        assert_eq!(
            sized("https://lh3.googleusercontent.com/a-/ALV-abc=s100"),
            Some("https://lh3.googleusercontent.com/a-/ALV-abc=s256-c".to_string())
        );
        // No options suffix: one is appended.
        assert_eq!(
            sized("https://lh3.googleusercontent.com/a-/ALV-abc"),
            Some("https://lh3.googleusercontent.com/a-/ALV-abc=s256-c".to_string())
        );
        for bad in [
            "http://lh3.googleusercontent.com/a=s100",
            "https://googleusercontent.com.evil.example/a=s100",
            "https://evilgoogleusercontent.com/a=s100",
            "https://169.254.169.254/latest/meta-data",
            "http://127.0.0.1:8848/client/stats",
            "https://lh3.googleusercontent.com/a?next=http://10.0.0.1/",
            "not a url",
        ] {
            assert_eq!(sized(bad), None, "{bad}");
        }
    }

    #[test]
    fn the_type_comes_from_the_bytes() {
        assert_eq!(sniff_image(JPEG), Some("image/jpeg"));
        assert_eq!(sniff_image(b"\x89PNG\r\n\x1a\nrest"), Some("image/png"));
        assert_eq!(sniff_image(b"GIF89a..."), Some("image/gif"));
        assert_eq!(sniff_image(b"RIFF\0\0\0\0WEBPVP8 "), Some("image/webp"));
        assert_eq!(sniff_image(b"<svg onload=alert(1)>"), None);
        assert_eq!(sniff_image(b"<!doctype html>"), None);
        assert_eq!(sniff_image(b""), None);
    }

    #[test]
    fn a_refusal_says_which_of_the_two_fixes_it_needs() {
        let disabled = br#"{"error":{"code":403,"status":"PERMISSION_DENIED","message":"People API has not been used in project 1234 before or it is disabled.","details":[{"@type":"type.googleapis.com/google.rpc.ErrorInfo","reason":"SERVICE_DISABLED","domain":"googleapis.com"}]}}"#;
        let msg = refusal_message(403, disabled);
        assert!(msg.contains("not enabled"), "{msg}");
        // The project number in Google's prose never reaches the log line.
        assert!(!msg.contains("1234"), "{msg}");

        let scope =
            br#"{"error":{"code":403,"details":[{"reason":"ACCESS_TOKEN_SCOPE_INSUFFICIENT"}]}}"#;
        assert!(refusal_message(403, scope).contains("squelchd auth"));

        // Anything else is reported as Google named it, and only if it is
        // shaped like one of Google's constants.
        let other = br#"{"error":{"details":[{"reason":"RATE_LIMIT_EXCEEDED"},{"reason":"<b>not a reason</b>"}]}}"#;
        assert_eq!(
            refusal_message(403, other),
            "the People API refused with 403: RATE_LIMIT_EXCEEDED"
        );
        assert!(refusal_message(401, b"<html>").contains("gave no reason"));
    }

    /// What the mock People API and image host answer with, and what they saw.
    #[derive(Default)]
    struct Script {
        person: Option<(u16, Value)>,
        photo: Vec<u8>,
        photo_auth: Vec<Option<String>>,
        photo_query: Vec<String>,
        people_calls: usize,
    }
    type Shared = Arc<Mutex<Script>>;

    async fn mock_person(State(s): State<Shared>) -> Response {
        let mut st = s.lock().unwrap();
        st.people_calls += 1;
        match st.person.clone() {
            Some((status, body)) => {
                (StatusCode::from_u16(status).unwrap(), Json(body)).into_response()
            }
            None => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        }
    }

    async fn mock_photo(
        State(s): State<Shared>,
        axum::extract::Path(tail): axum::extract::Path<String>,
        headers: axum::http::HeaderMap,
    ) -> Response {
        let mut st = s.lock().unwrap();
        st.photo_auth.push(
            headers
                .get("authorization")
                .map(|v| v.to_str().unwrap_or_default().to_string()),
        );
        st.photo_query.push(tail);
        st.photo.clone().into_response()
    }

    struct FixedToken;

    #[async_trait]
    impl CredentialStore for FixedToken {
        async fn token(&self, _account: AccountId) -> CoreResult<crate::credentials::OAuthToken> {
            Ok(crate::credentials::OAuthToken {
                access_token: "test-token".to_string(),
                refresh_token: None,
                expires_at: None,
            })
        }
    }

    async fn rig() -> (
        Shared,
        String,
        Arc<SqliteStore>,
        AccountId,
        SyncEngine<SqliteStore, FixedToken>,
    ) {
        let script: Shared = Arc::default();
        let app = Router::new()
            .route("/people/me", get(mock_person))
            .route("/photo/{tail}", get(mock_photo))
            .with_state(script.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let store = Arc::new(SqliteStore::open_in_memory().unwrap());
        let acct = store.ensure_account("me@example.com").unwrap();
        let mut engine = SyncEngine::new(
            store.clone(),
            Arc::new(FixedToken),
            acct,
            "me@example.com".to_string(),
            Config::default(),
        );
        engine.profile_url = format!("{base}/people/me");
        (script, base, store, acct, engine)
    }

    fn ada(base: &str) -> Value {
        json!({
            "names": [{"metadata": {"primary": true}, "displayName": "Ada Lovelace", "givenName": "Ada"}],
            "photos": [{"metadata": {"primary": true}, "url": format!("{base}/photo/ada=s100")}],
        })
    }

    #[tokio::test]
    async fn a_profile_is_fetched_stored_and_not_refetched_until_due() {
        let (script, base, store, acct, engine) = rig().await;
        {
            let mut st = script.lock().unwrap();
            st.person = Some((200, ada(&base)));
            st.photo = JPEG.to_vec();
        }

        engine.refresh_profile().await;
        let who = store.account_identity(acct).unwrap().expect("stored");
        assert_eq!(who.name.as_deref(), Some("Ada Lovelace"));
        assert_eq!(who.given_name.as_deref(), Some("Ada"));
        let photo = store.account_photo(acct).unwrap().expect("photo stored");
        assert_eq!(photo.bytes, JPEG);
        assert_eq!(photo.mime, "image/jpeg");
        assert_eq!(who.photo_tag.as_deref(), Some(photo.tag.as_str()));

        {
            let st = script.lock().unwrap();
            // The bearer token went to the People API and NOT to the image host.
            assert_eq!(st.photo_auth, vec![None]);
            // ...which was asked for our size, not the one the answer named.
            assert_eq!(st.photo_query, vec!["ada=s256-c".to_string()]);
        }

        // Every later poll tick is a no-op until the refresh comes due.
        engine.refresh_profile().await;
        engine.refresh_profile().await;
        assert_eq!(script.lock().unwrap().people_calls, 1);
    }

    #[tokio::test]
    async fn a_grant_without_the_scope_stores_nothing_and_retries_later() {
        let (script, _base, store, acct, engine) = rig().await;
        script.lock().unwrap().person =
            Some((403, json!({"error": {"status": "PERMISSION_DENIED"}})));

        engine.refresh_profile().await;
        assert_eq!(store.account_identity(acct).unwrap(), None);
        assert_eq!(store.account_photo(acct).unwrap(), None);
        // Backed off: the next tick does not ask again.
        engine.refresh_profile().await;
        assert_eq!(script.lock().unwrap().people_calls, 1);
        let due_in = engine.profile_due.load(Ordering::Relaxed) - Utc::now().timestamp();
        assert!(due_in > 0 && due_in <= RETRY_EVERY_SECS, "{due_in}");
    }

    #[tokio::test]
    async fn a_failed_refresh_keeps_the_last_profile_and_a_removed_photo_is_cleared() {
        let (script, base, store, acct, engine) = rig().await;
        {
            let mut st = script.lock().unwrap();
            st.person = Some((200, ada(&base)));
            st.photo = JPEG.to_vec();
        }
        engine.refresh_profile().await;
        let before = store.account_identity(acct).unwrap().unwrap();
        assert!(before.photo_tag.is_some());

        // The image host starts answering with something that is not an image:
        // the whole refresh is abandoned, name included, and the old row stands.
        script.lock().unwrap().photo = b"<!doctype html>".to_vec();
        engine.profile_due.store(0, Ordering::Relaxed);
        engine.refresh_profile().await;
        assert_eq!(store.account_identity(acct).unwrap().unwrap(), before);
        assert_eq!(store.account_photo(acct).unwrap().unwrap().bytes, JPEG);

        // The person removes their picture: Google now reports the default, and
        // the stored photo goes with it while the name stays.
        script.lock().unwrap().person = Some((
            200,
            json!({
                "names": [{"displayName": "Ada Lovelace", "givenName": "Ada"}],
                "photos": [{"url": format!("{base}/photo/ada=s100"), "default": true}],
            }),
        ));
        engine.profile_due.store(0, Ordering::Relaxed);
        engine.refresh_profile().await;
        let after = store.account_identity(acct).unwrap().unwrap();
        assert_eq!(after.name.as_deref(), Some("Ada Lovelace"));
        assert_eq!(after.photo_tag, None);
        assert_eq!(store.account_photo(acct).unwrap(), None);
    }
}
