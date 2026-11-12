//! `GET /client/image?u=<url>`: a remote image in a mail body, fetched by the
//! daemon on the reader's behalf.
//!
//! SPIKE (web client). A browser cannot fetch a sender's image the way the Mac
//! app does (no cookies, no Referer, no connection from the reader's address),
//! so the web client points every remote `<img>` here instead. The rules:
//!
//!   - inside the bearer-authed `/client` tree, like every other byte this door
//!     serves;
//!   - outbound through the same guard as the automatic unsubscribe
//!     ([`crate::unsub_fetch`]): http/https only, ports 80/443, no userinfo,
//!     names resolve to public addresses only, IP literals judged directly;
//!   - no cookie jar, no Referer, one generic User-Agent, `Accept: image/*`;
//!   - http tried as https first, as the unsubscribe fetcher does;
//!   - redirects followed BY HAND, a few hops, each hop re-checked;
//!   - a byte cap and a timeout, and a bound on how many run at once;
//!   - the type comes from the BYTES, never the header: raster formats only,
//!     and SVG is never served (it has no magic number, only markup);
//!   - served through [`crate::handlers::attachment_response`], so the same
//!     `nosniff` and content-type discipline as attachments.
//!
//! Errors never echo the URL: it can carry a sender's per-recipient token.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use axum::extract::Query;
use axum::http::StatusCode;
use axum::response::Response;
use serde::Deserialize;
use tokio::sync::Semaphore;
use url::Url;

use crate::error::ApiError;
use crate::unsub_fetch::{PublicOnly, check_url, redact};

/// The largest image served. A mail image past this is a photo attachment in
/// disguise, and the reader can open the mail elsewhere.
pub(crate) const MAX_IMAGE_BYTES: usize = 8 * 1024 * 1024;
const MAX_REDIRECTS: usize = 3;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Fetches in flight at once, daemon-wide. A 50-image newsletter queues
/// behind this rather than opening 50 sockets.
const CONCURRENCY: usize = 8;
/// Generic on purpose: the point of fetching here is that the sender learns as
/// little about the reader as a fetch can tell it.
const USER_AGENT: &str = "Mozilla/5.0 (compatible; image fetcher)";

/// Bytes [`sniff`] reads. Fewer than this and the answer can still change.
const SNIFF_LEN: usize = 12;

#[derive(Debug, Deserialize)]
pub struct ImageQuery {
    u: String,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ProxyError {
    /// The URL or a hop of it points somewhere this fetcher will not go.
    Refused(&'static str),
    /// The far side answered, but not with an image we serve.
    NotImage,
    TooLarge,
    Upstream(u16),
    Transport(String),
}

impl From<ProxyError> for ApiError {
    fn from(e: ProxyError) -> Self {
        match e {
            ProxyError::Refused(why) => ApiError::new(StatusCode::FORBIDDEN, why),
            ProxyError::NotImage => {
                ApiError::new(StatusCode::UNSUPPORTED_MEDIA_TYPE, "not an image")
            }
            ProxyError::TooLarge => ApiError::new(StatusCode::PAYLOAD_TOO_LARGE, "image too large"),
            ProxyError::Upstream(_) => ApiError::new(StatusCode::BAD_GATEWAY, "upstream refused"),
            ProxyError::Transport(kind) => {
                ApiError::new(StatusCode::BAD_GATEWAY, format!("upstream {kind}"))
            }
        }
    }
}

/// The raster type these bytes open with, or None. A port of the Mac client's
/// `ImageSniff`: RASTER ONLY, and that is the security rule.
pub(crate) fn sniff(head: &[u8]) -> Option<&'static str> {
    let opens = |magic: &[u8], at: usize| {
        head.len() >= at + magic.len() && &head[at..at + magic.len()] == magic
    };
    if opens(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A], 0) {
        return Some("image/png");
    }
    if opens(&[0xFF, 0xD8, 0xFF], 0) {
        return Some("image/jpeg");
    }
    if opens(b"GIF87a", 0) || opens(b"GIF89a", 0) {
        return Some("image/gif");
    }
    if opens(b"RIFF", 0) && opens(b"WEBP", 8) {
        return Some("image/webp");
    }
    if opens(b"ftyp", 4) && head.len() >= 12 {
        return match &head[8..12] {
            b"avif" | b"avis" => Some("image/avif"),
            // HEIC is left out: no browser but Safari decodes it, so serving it
            // to a web client is bytes for nothing.
            _ => None,
        };
    }
    None
}

/// One fetcher. The production one is shared (it holds no per-reader state:
/// no jar); tests build their own.
pub(crate) struct ImageFetcher {
    client: reqwest::Client,
    allow_local: bool,
    /// Tests only: an exact `host:port` that skips the per-hop check, so a
    /// loopback stub can serve the FIRST hop while later hops meet production
    /// rules (same device as `unsub_fetch::Fetcher`).
    #[cfg(test)]
    trusted: Option<String>,
}

impl ImageFetcher {
    pub(crate) fn new(allow_local: bool) -> Result<Self, ProxyError> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .dns_resolver(Arc::new(PublicOnly { allow_local }))
            .no_proxy()
            .timeout(REQUEST_TIMEOUT)
            .connect_timeout(CONNECT_TIMEOUT)
            .user_agent(USER_AGENT)
            .build()
            .map_err(|e| ProxyError::Transport(redact(&e)))?;
        Ok(Self {
            client,
            allow_local,
            #[cfg(test)]
            trusted: None,
        })
    }

    fn check_hop(&self, url: &Url) -> Result<(), ProxyError> {
        #[cfg(test)]
        if let Some(t) = &self.trusted {
            let authority = format!(
                "{}:{}",
                url.host_str().unwrap_or_default(),
                url.port_or_known_default().unwrap_or_default()
            );
            if &authority == t {
                return Ok(());
            }
        }
        check_url(url, self.allow_local).map_err(|_| ProxyError::Refused("address"))
    }

    fn is_trusted(&self, _url: &Url) -> bool {
        #[cfg(test)]
        if let Some(t) = &self.trusted {
            return format!(
                "{}:{}",
                _url.host_str().unwrap_or_default(),
                _url.port_or_known_default().unwrap_or_default()
            ) == *t;
        }
        false
    }

    /// Fetch `raw` and return `(mime, bytes)` for a raster image within the cap.
    pub(crate) async fn fetch(&self, raw: &str) -> Result<(&'static str, Vec<u8>), ProxyError> {
        let mut url = Url::parse(raw.trim()).map_err(|_| ProxyError::Refused("not a url"))?;
        for _ in 0..=MAX_REDIRECTS {
            self.check_hop(&url)?;
            let mut resp = self.send(&url).await?;
            let status = resp.status();
            if status.is_redirection() {
                let loc = resp
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .ok_or(ProxyError::Refused("bad redirect"))?;
                url = url
                    .join(loc)
                    .map_err(|_| ProxyError::Refused("bad redirect"))?;
                continue;
            }
            if !status.is_success() {
                return Err(ProxyError::Upstream(status.as_u16()));
            }
            if let Some(len) = resp.content_length()
                && len as usize > MAX_IMAGE_BYTES
            {
                return Err(ProxyError::TooLarge);
            }
            let mut bytes: Vec<u8> = Vec::new();
            let mut mime = None;
            while let Some(chunk) = resp
                .chunk()
                .await
                .map_err(|e| ProxyError::Transport(redact(&e)))?
            {
                if bytes.len() + chunk.len() > MAX_IMAGE_BYTES {
                    return Err(ProxyError::TooLarge);
                }
                bytes.extend_from_slice(&chunk);
                // Decide as soon as the prefix is in: a non-image is dropped
                // without reading the rest of it.
                if mime.is_none() && bytes.len() >= SNIFF_LEN {
                    mime = Some(sniff(&bytes).ok_or(ProxyError::NotImage)?);
                }
            }
            return match mime {
                Some(m) => Ok((m, bytes)),
                None => sniff(&bytes)
                    .map(|m| (m, bytes))
                    .ok_or(ProxyError::NotImage),
            };
        }
        Err(ProxyError::Refused("too many redirects"))
    }

    /// One hop, https first for an http URL (connect failure falls back).
    async fn send(&self, url: &Url) -> Result<reqwest::Response, ProxyError> {
        if url.scheme() == "http" && !self.allow_local && !self.is_trusted(url) {
            let mut secure = url.clone();
            if secure.set_scheme("https").is_ok() && secure.set_port(None).is_ok() {
                match self.get(&secure).await {
                    Ok(r) => return Ok(r),
                    Err(e) if e.is_connect() => {}
                    Err(e) => return Err(ProxyError::Transport(redact(&e))),
                }
            }
        }
        self.get(url)
            .await
            .map_err(|e| ProxyError::Transport(redact(&e)))
    }

    async fn get(&self, url: &Url) -> Result<reqwest::Response, reqwest::Error> {
        self.client
            .get(url.clone())
            .header(
                reqwest::header::ACCEPT,
                "image/avif,image/webp,image/png,image/jpeg,image/gif,image/*;q=0.8",
            )
            .send()
            .await
    }
}

fn shared() -> Result<&'static ImageFetcher, ApiError> {
    static FETCHER: OnceLock<Option<ImageFetcher>> = OnceLock::new();
    FETCHER
        .get_or_init(|| ImageFetcher::new(false).ok())
        .as_ref()
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "image fetcher unavailable",
            )
        })
}

fn permits() -> &'static Semaphore {
    static SEM: OnceLock<Semaphore> = OnceLock::new();
    SEM.get_or_init(|| Semaphore::new(CONCURRENCY))
}

pub async fn get_image(Query(q): Query<ImageQuery>) -> Result<Response, ApiError> {
    let fetcher = shared()?;
    let _permit = permits()
        .acquire()
        .await
        .map_err(|_| ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "busy"))?;
    let (mime, bytes) = fetcher.fetch(&q.u).await?;
    Ok(crate::handlers::attachment_response(
        "image",
        mime,
        bytes,
        // A sender's image can change under the same URL, but not usefully
        // within a reading session; a day keeps a reopened mail instant.
        "private, max-age=86400",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    const PNG: &[u8] = &[
        0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13, b'I', b'H', b'D', b'R',
    ];

    /// A one-shot-per-connection HTTP stub on loopback answering every request
    /// with `response`. Returns its `host:port`.
    async fn stub(response: Vec<u8>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                let response = response.clone();
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    let _ = sock.read(&mut buf).await;
                    let _ = sock.write_all(&response).await;
                    let _ = sock.shutdown().await;
                });
            }
        });
        addr
    }

    fn http(status: &str, headers: &[(&str, &str)], body: &[u8]) -> Vec<u8> {
        let mut out = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n",
            body.len()
        );
        for (k, v) in headers {
            out.push_str(&format!("{k}: {v}\r\n"));
        }
        out.push_str("\r\n");
        let mut out = out.into_bytes();
        out.extend_from_slice(body);
        out
    }

    fn trusting(authority: &str) -> ImageFetcher {
        let mut f = ImageFetcher::new(false).unwrap();
        f.trusted = Some(authority.to_string());
        f
    }

    #[test]
    fn sniff_accepts_raster_and_refuses_everything_else() {
        assert_eq!(sniff(PNG), Some("image/png"));
        assert_eq!(
            sniff(&[0xFF, 0xD8, 0xFF, 0xE0, 0, 0, 0, 0, 0, 0, 0, 0]),
            Some("image/jpeg")
        );
        assert_eq!(sniff(b"GIF89a\0\0\0\0\0\0"), Some("image/gif"));
        assert_eq!(sniff(b"RIFF\0\0\0\0WEBPVP8 "), Some("image/webp"));
        assert_eq!(sniff(b"\0\0\0\x1cftypavif"), Some("image/avif"));
        // Same container, video brand.
        assert_eq!(sniff(b"\0\0\0\x1cftypisom"), None);
        // RIFF alone is also wav.
        assert_eq!(sniff(b"RIFF\0\0\0\0WAVEfmt "), None);
        // SVG, HTML and a header-only lie all refused.
        assert_eq!(sniff(b"<svg xmlns=\"http://www.w3.org/2000/svg\">"), None);
        assert_eq!(sniff(b"<!doctype html><script>"), None);
        assert_eq!(sniff(b""), None);
    }

    #[tokio::test]
    async fn refuses_private_loopback_link_local_and_metadata_literals() {
        let f = ImageFetcher::new(false).unwrap();
        for url in [
            "http://127.0.0.1/a.png",
            "http://10.0.0.5/a.png",
            "http://192.168.1.1/a.png",
            "http://169.254.169.254/latest/meta-data/",
            "http://[::1]/a.png",
            "http://[fe80::1]/a.png",
            "http://[::ffff:127.0.0.1]/a.png",
            "http://100.64.0.1/a.png",
            "http://localhost/a.png",
            "http://metadata.google.internal/a.png",
            "http://intranet/a.png",
            "https://images.example:8443/a.png",
            "https://user:pw@images.example/a.png",
            "file:///etc/passwd",
            "data:image/png;base64,AAAA",
            "not a url",
        ] {
            assert!(
                matches!(f.fetch(url).await, Err(ProxyError::Refused(_))),
                "{url} should be refused before any connection"
            );
        }
    }

    #[tokio::test]
    async fn serves_a_sniffed_png_whatever_the_header_claims() {
        let host = stub(http(
            "200 OK",
            &[("Content-Type", "application/octet-stream")],
            PNG,
        ))
        .await;
        let f = trusting(&host);
        let (mime, bytes) = f.fetch(&format!("http://{host}/a.png")).await.unwrap();
        assert_eq!(mime, "image/png");
        assert_eq!(bytes, PNG);
    }

    #[tokio::test]
    async fn refuses_svg_and_html_even_when_labelled_image() {
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg"><script>alert(1)</script></svg>"#;
        let host = stub(http("200 OK", &[("Content-Type", "image/svg+xml")], svg)).await;
        assert_eq!(
            trusting(&host).fetch(&format!("http://{host}/x.svg")).await,
            Err(ProxyError::NotImage)
        );
        let host = stub(http(
            "200 OK",
            &[("Content-Type", "image/png")],
            b"<!doctype html><p>hi",
        ))
        .await;
        assert_eq!(
            trusting(&host).fetch(&format!("http://{host}/x.png")).await,
            Err(ProxyError::NotImage)
        );
    }

    #[tokio::test]
    async fn a_redirect_into_loopback_is_refused_at_the_hop() {
        // The first hop is the trusted stub; it redirects to another loopback
        // port, which must meet the production check and fail it.
        let target = stub(http("200 OK", &[], PNG)).await;
        let host = stub(http(
            "302 Found",
            &[("Location", &format!("http://{target}/a.png"))],
            b"",
        ))
        .await;
        assert_eq!(
            trusting(&host).fetch(&format!("http://{host}/a.png")).await,
            Err(ProxyError::Refused("address"))
        );
        // And to the metadata address by literal.
        let host = stub(http(
            "302 Found",
            &[("Location", "http://169.254.169.254/")],
            b"",
        ))
        .await;
        assert_eq!(
            trusting(&host).fetch(&format!("http://{host}/a.png")).await,
            Err(ProxyError::Refused("address"))
        );
    }

    #[tokio::test]
    async fn the_byte_cap_holds_with_and_without_content_length() {
        let mut big = PNG.to_vec();
        big.resize(MAX_IMAGE_BYTES + 1, 0);
        let host = stub(http("200 OK", &[], &big)).await;
        assert_eq!(
            trusting(&host)
                .fetch(&format!("http://{host}/big.png"))
                .await,
            Err(ProxyError::TooLarge)
        );
        // No Content-Length: the stream is counted instead.
        let mut raw = b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n".to_vec();
        raw.extend_from_slice(&big);
        let host = stub(raw).await;
        assert_eq!(
            trusting(&host)
                .fetch(&format!("http://{host}/big.png"))
                .await,
            Err(ProxyError::TooLarge)
        );
    }

    #[tokio::test]
    async fn sends_no_cookie_no_referer_and_the_generic_agent() {
        // A stub that echoes the request head back as the failure detail.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let host = listener.local_addr().unwrap().to_string();
        let (tx, rx) = tokio::sync::oneshot::channel::<String>();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 8192];
            let n = sock.read(&mut buf).await.unwrap();
            let _ = tx.send(String::from_utf8_lossy(&buf[..n]).to_lowercase());
            let _ = sock.write_all(&http("200 OK", &[], PNG)).await;
        });
        trusting(&host)
            .fetch(&format!("http://{host}/a.png"))
            .await
            .unwrap();
        let head = rx.await.unwrap();
        assert!(!head.contains("\r\ncookie:"), "{head}");
        assert!(!head.contains("\r\nreferer:"), "{head}");
        assert!(!head.contains("authorization:"), "{head}");
        assert!(
            head.contains(&format!("user-agent: {}", USER_AGENT.to_lowercase())),
            "{head}"
        );
    }

    #[tokio::test]
    async fn upstream_errors_do_not_echo_the_url() {
        let host = stub(http("404 Not Found", &[], b"")).await;
        let url = format!("http://{host}/track?recipient=secret-token-123");
        let err = trusting(&host).fetch(&url).await.unwrap_err();
        let api: ApiError = err.into();
        assert!(!api.message.contains("secret-token-123"));
        assert_eq!(api.status, StatusCode::BAD_GATEWAY);
    }
}
