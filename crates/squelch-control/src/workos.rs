//! The WorkOS client: one outbound call that finishes an agent's AuthKit login.
//!
//! WorkOS AuthKit is the OAuth authorization server agents use to reach a
//! tenant's `/mcp`, in "Standalone Connect" mode: AuthKit runs the OAuth dance
//! with the agent and DELEGATES the login to us. It sends the person's browser
//! to `GET /agent/auth?external_auth_id=...` (see
//! [`crate::handlers::agent_auth`]), we prove who they are through the same
//! Google identity hop the app login uses, and then this module tells AuthKit
//! who that was: `POST /authkit/oauth2/complete`, answered with the URL the
//! browser goes to next.
//!
//! WHAT AUTHKIT IS TOLD IS THE WHOLE SECURITY STORY. The `email` sent here
//! becomes the `email` claim in the access token, and that claim is the only
//! thing a tenant daemon holds against its own account to decide whether a
//! token may read its mail (the contract's I1). So the address comes from
//! Google's verified answer plus this control plane's own tenant record, and
//! never from anything the browser sent.
//!
//! The same outbound discipline as every other client in this crate:
//! redirects refused, one timeout, response bodies capped, and errors whose
//! `Display` names nothing.
//!
//! WHAT MUST NOT LEAK:
//!
//! - Nothing in this module logs. The caller logs an outcome category and, on
//!   success, the redirect's HOST.
//! - [`WorkosError`]'s `Display` carries no key, no `external_auth_id`, no
//!   address, no status, and nothing WorkOS said. Its response body is parsed
//!   for one field and otherwise dropped, and it never reaches a browser.
//! - [`WorkosClient`] derives nothing: it holds the API key.

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// The budget for the one completion call. Tighter than the crate's general
/// [`crate::config::OUTBOUND_TIMEOUT`]: a person is sitting on a blank tab
/// between Google and AuthKit's consent screen while this runs, and a WorkOS
/// that has not answered in ten seconds is a WorkOS to say "try again" about.
pub const COMPLETE_TIMEOUT: Duration = Duration::from_secs(10);

/// Ceiling on a response body. The completion answers with a small JSON object
/// carrying one URL; anything bigger is not the API we know.
const MAX_RESPONSE_BODY: usize = 64 * 1024;

/// Ceiling on the URL AuthKit hands back. It carries AuthKit's own state, so it
/// is not tiny, but a URL longer than this is not one a browser should be sent
/// to and not one that fits comfortably in a `Location` header.
const MAX_REDIRECT_LEN: usize = 4096;

#[derive(Debug, thiserror::Error)]
pub enum WorkosError {
    /// WorkOS could not be reached, timed out, answered with its own failure
    /// (5xx, or its rate limit), or answered something that is not the JSON we
    /// know. Worth trying again.
    #[error("WorkOS could not be reached or answered unreadably")]
    Unreachable,
    /// 401/403. A deployment misconfiguration worth shouting about: no agent
    /// connects to anything until the key is fixed.
    #[error("WorkOS refused our API key")]
    Unauthorized,
    /// Any other 4xx: most often an `external_auth_id` that is unknown, spent,
    /// or expired at AuthKit. Retrying in the same browser can never work; the
    /// agent's flow has to start again.
    #[error("WorkOS refused to complete the login")]
    Refused,
    /// A 2xx whose `redirect_uri` is not one this service will send a browser
    /// to: not absolute, not https, or carrying userinfo. See
    /// [`acceptable_redirect`].
    #[error("WorkOS answered with a redirect this service will not follow")]
    BadRedirect,
}

/// `POST /authkit/oauth2/complete` request body. The field names are WorkOS's.
#[derive(Serialize)]
struct CompleteRequest<'a> {
    external_auth_id: &'a str,
    user: CompleteUser<'a>,
}

/// The user AuthKit is told about. Two fields and no more: no name, no
/// metadata, nothing the token does not need. Every extra field here is one
/// more fact about a customer handed to a third party.
#[derive(Serialize)]
struct CompleteUser<'a> {
    /// Stable identifier for the user at AuthKit. The mailbox, lowercased: one
    /// mailbox is one daemon is one user, and folding the case means a Google
    /// answer in another capitalization is the same AuthKit user rather than a
    /// second one.
    id: &'a str,
    /// The mailbox exactly as the tenant record stores it. This becomes the
    /// token's `email` claim, which the daemon compares against its own account.
    email: &'a str,
}

#[derive(Deserialize)]
struct CompleteResponse {
    redirect_uri: String,
}

/// The client: one reqwest client, one API key, one base URL.
///
/// DELIBERATELY DERIVES NOTHING, the way [`crate::resend::ResendClient`] does:
/// the key lives here for the process's lifetime and must not be formattable.
pub struct WorkosClient {
    base_url: String,
    /// `Bearer <key>`, precomputed once. As secret as the key it carries.
    auth_header: String,
    http: reqwest::Client,
}

impl WorkosClient {
    /// `base_url` is a canonical origin (no trailing slash) and `api_key` the
    /// WorkOS API key; both come from [`crate::config::WorkosConfig`].
    pub fn new(base_url: String, api_key: String, timeout: Duration) -> Result<Self, WorkosError> {
        let http = reqwest::Client::builder()
            // Redirects refused: the request carries the API KEY, and a
            // redirect is how it ends up at a host nobody chose. (The URL in the
            // ANSWER is a different thing: it is for the browser, and it is
            // checked before anybody is sent to it.)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(timeout)
            .build()
            .map_err(|_| WorkosError::Unreachable)?;
        Ok(Self {
            base_url,
            auth_header: format!("Bearer {api_key}"),
            http,
        })
    }

    /// Tell AuthKit who completed the login for `external_auth_id`, and get
    /// back the URL the browser goes to next.
    ///
    /// The answer is checked with [`acceptable_redirect`] before it is
    /// returned, so a caller holding an `Ok` holds a URL it may send a browser
    /// to. `allow_http` is for a local development run and nothing else.
    pub async fn complete(
        &self,
        external_auth_id: &str,
        user_id: &str,
        email: &str,
        allow_http: bool,
    ) -> Result<url::Url, WorkosError> {
        let resp = self
            .http
            .post(format!("{}/authkit/oauth2/complete", self.base_url))
            .header(reqwest::header::AUTHORIZATION, &self.auth_header)
            .json(&CompleteRequest {
                external_auth_id,
                user: CompleteUser { id: user_id, email },
            })
            .send()
            .await
            .map_err(|_| WorkosError::Unreachable)?;

        match resp.status().as_u16() {
            200..=299 => {}
            401 | 403 => return Err(WorkosError::Unauthorized),
            // WorkOS's own trouble, and its rate limit: the same request can
            // succeed in a minute, so these say "try again".
            429 | 500..=599 => return Err(WorkosError::Unreachable),
            // Everything else is AuthKit refusing THIS authorization, which a
            // retry of the same id will not change.
            _ => return Err(WorkosError::Refused),
        }
        let body = read_capped(resp).await?;
        let parsed: CompleteResponse =
            serde_json::from_slice(&body).map_err(|_| WorkosError::Unreachable)?;
        acceptable_redirect(&parsed.redirect_uri, allow_http).ok_or(WorkosError::BadRedirect)
    }
}

/// Whether a URL from AuthKit is one this service will send a browser to.
///
/// THIS IS THE ONE PLACE A THIRD PARTY'S STRING BECOMES A `Location`, so it is
/// held to a shape rather than trusted because the call that produced it was
/// authenticated: an absolute URL, `https` (plain `http` only on a local
/// development run of this service), a host, and no userinfo. That refuses
/// `javascript:` and `data:` outright, a relative path that would land on THIS
/// origin, a scheme-relative `//host`, and the `https://trusted@evil` shape that
/// reads as one host and goes to another.
///
/// It does NOT pin the host to an AuthKit domain. The AuthKit domain is the
/// tenant's issuer, configured on the daemons and not here, and the answer
/// arrives over a TLS connection to WorkOS authenticated by our key: what this
/// guards against is a malformed or hostile STRING, not a hostile WorkOS.
pub fn acceptable_redirect(raw: &str, allow_http: bool) -> Option<url::Url> {
    if raw.is_empty() || raw.len() > MAX_REDIRECT_LEN {
        return None;
    }
    // `Url::parse` refuses a relative reference, so "absolute" is this line.
    let url = url::Url::parse(raw).ok()?;
    let scheme_ok = match url.scheme() {
        "https" => true,
        "http" => allow_http,
        _ => false,
    };
    if !scheme_ok
        || url.host_str().is_none_or(str::is_empty)
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return None;
    }
    Some(url)
}

async fn read_capped(mut resp: reqwest::Response) -> Result<Vec<u8>, WorkosError> {
    let mut out = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(|_| WorkosError::Unreachable)? {
        if out.len() + chunk.len() > MAX_RESPONSE_BODY {
            return Err(WorkosError::Unreachable);
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape gate, one case per way a string can try to be a destination.
    #[test]
    fn only_an_absolute_https_url_is_a_destination() {
        for good in [
            "https://foo.authkit.app/oauth2/authorize?x=1",
            "https://foo.authkit.app",
        ] {
            assert!(acceptable_redirect(good, false).is_some(), "{good}");
        }
        for bad in [
            "",
            "javascript:alert(1)",
            "JAVASCRIPT:alert(1)",
            "data:text/html,<script>alert(1)</script>",
            "http://foo.authkit.app/x",
            "/relative/path",
            "relative",
            "//evil.test/x",
            "https://foo.authkit.app@evil.test/x",
            "https://user:pw@foo.authkit.app/x",
            "ftp://foo.authkit.app/x",
            "https://",
        ] {
            assert!(acceptable_redirect(bad, false).is_none(), "{bad:?}");
        }
        let long = format!("https://foo.authkit.app/{}", "a".repeat(MAX_REDIRECT_LEN));
        assert!(acceptable_redirect(&long, false).is_none());
    }

    /// Plain http is a local development affordance, and only that: the same
    /// string is refused on a deployment served over https.
    #[test]
    fn plain_http_is_allowed_only_on_an_insecure_run() {
        assert!(acceptable_redirect("http://127.0.0.1:9/x", false).is_none());
        assert!(acceptable_redirect("http://127.0.0.1:9/x", true).is_some());
        // ...and the relaxation is about the scheme alone.
        assert!(acceptable_redirect("javascript:alert(1)", true).is_none());
        assert!(acceptable_redirect("/relative", true).is_none());
    }

    /// Every error renders without a status code, a key, or anything else that
    /// would be worth hiding.
    #[test]
    fn errors_name_nothing() {
        for e in [
            WorkosError::Unreachable,
            WorkosError::Unauthorized,
            WorkosError::Refused,
            WorkosError::BadRedirect,
        ] {
            let s = e.to_string();
            assert!(!s.chars().any(|c| c.is_ascii_digit()), "{s}");
            assert!(!s.contains("sk_"), "{s}");
        }
    }
}
