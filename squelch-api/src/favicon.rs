//! `GET /client/favicon?domain=<d>`: a brand sender's logo, fetched by the
//! daemon so the reader's browser never contacts an icon service.
//!
//! The source is the one the Mac client uses (`SenderID.faviconURL` in
//! passband/Sources/Passband/Lib/SenderIdentity.swift), with its alias table,
//! and the same verdict: only a 200 carrying a real image bigger than one
//! pixel is a logo. Anything else is a MISS, answered 404 so the client draws
//! initials, and remembered for a while so a dead domain is not asked again
//! on every render.
//!
//! The fetch goes through the image proxy's fetcher, so the same outbound
//! guard, byte cap, timeout, rate limit and cache apply; ICO is accepted here
//! (icon services answer with it), SVG never.
//!
//! THE CLIENT DECIDES WHICH DOMAINS ARE ASKED FOR. The server cannot tell a
//! brand from a person, so a client must only ever ask for a robot or brand
//! sender's domain, never a human correspondent's own (the web client's
//! `eligibleFaviconDomain`, the Mac's `SenderID.eligibleFaviconDomain`).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use axum::extract::Query;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use crate::error::ApiError;
use crate::image_proxy::{
    Accept, ImageFetcher, ProxyError, bucket, cache, permits, pixel_size, shared, too_many,
};

/// Where logos come from, as on the Mac.
const SOURCE: &str = "https://icons.duckduckgo.com/ip3/";

/// How long a miss is believed, and how many are kept.
const MISS_TTL: Duration = Duration::from_secs(6 * 60 * 60);
const MISS_CAP: usize = 4096;

/// A logo does not change within a week in any way a reader would notice.
const CACHE_CONTROL: &str = "private, max-age=604800";

/// Sending domains whose brand lives on another site (the Mac's
/// `SenderID.logoAliases`).
const ALIASES: &[(&str, &str)] = &[
    ("awsapps.com", "aws.amazon.com"),
    ("signin.aws", "aws.amazon.com"),
    ("amazonaws.com", "aws.amazon.com"),
];

#[derive(Debug, Deserialize)]
pub struct FaviconQuery {
    domain: String,
}

/// The domain to look up, or None unless `raw` is a plain DNS hostname:
/// letters, digits and hyphens in dot-separated labels (punycode `xn--`
/// included), at least two labels, no port, path, userinfo or IP literal.
pub(crate) fn favicon_domain(raw: &str) -> Option<String> {
    let d = raw.trim().to_ascii_lowercase();
    if d.len() < 3 || d.len() > 253 || !d.contains('.') {
        return None;
    }
    let labels: Vec<&str> = d.split('.').collect();
    for label in &labels {
        let ok = !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
        if !ok {
            return None;
        }
    }
    // An all-digit last label is an IPv4 literal (or nothing real).
    if labels.last()?.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let aliased = ALIASES
        .iter()
        .find(|(from, _)| *from == d)
        .map(|(_, to)| (*to).to_string());
    Some(aliased.unwrap_or(d))
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum FaviconError {
    /// No logo for this domain: draw initials.
    Miss,
    /// Could not tell (refused, unreachable, too large): not remembered.
    Proxy(ProxyError),
}

/// Fetch `domain`'s logo from `base` and judge it as the Mac does.
pub(crate) async fn fetch_favicon(
    fetcher: &ImageFetcher,
    base: &str,
    domain: &str,
) -> Result<(&'static str, Vec<u8>), FaviconError> {
    let url = format!("{base}{domain}.ico");
    match fetcher.fetch_as(&url, Accept::RasterOrIcon).await {
        Ok((mime, bytes)) => {
            // Blank or one-pixel answers are a service's fallback, not a logo.
            if let Some((w, h)) = pixel_size(mime, &bytes)
                && (w <= 1 || h <= 1)
            {
                return Err(FaviconError::Miss);
            }
            Ok((mime, bytes))
        }
        // The service's 404 (it answers unknown domains with one), or bytes
        // that are not an image we serve: no logo.
        Err(ProxyError::Upstream(_) | ProxyError::NotImage) => Err(FaviconError::Miss),
        Err(other) => Err(FaviconError::Proxy(other)),
    }
}

/// Domains recently found to have no logo.
#[derive(Default)]
pub(crate) struct Misses {
    at: HashMap<String, Instant>,
}

impl Misses {
    pub(crate) fn is_miss(&mut self, domain: &str, now: Instant) -> bool {
        match self.at.get(domain) {
            Some(t) if now.saturating_duration_since(*t) < MISS_TTL => true,
            Some(_) => {
                self.at.remove(domain);
                false
            }
            None => false,
        }
    }

    pub(crate) fn record(&mut self, domain: String, now: Instant) {
        if self.at.len() >= MISS_CAP {
            // Bounded, not clever: drop what has aged out, and if that is not
            // enough, start over. A forgotten miss costs one more lookup.
            self.at
                .retain(|_, t| now.saturating_duration_since(*t) < MISS_TTL);
            if self.at.len() >= MISS_CAP {
                self.at.clear();
            }
        }
        self.at.insert(domain, now);
    }
}

fn misses() -> &'static Mutex<Misses> {
    static MISSES: OnceLock<Mutex<Misses>> = OnceLock::new();
    MISSES.get_or_init(|| Mutex::new(Misses::default()))
}

pub async fn get_favicon(Query(q): Query<FaviconQuery>) -> Result<Response, ApiError> {
    let domain = favicon_domain(&q.domain)
        .ok_or_else(|| ApiError::new(StatusCode::BAD_REQUEST, "not a domain"))?;
    let now = Instant::now();
    if misses()
        .lock()
        .map(|mut m| m.is_miss(&domain, now))
        .unwrap_or(false)
    {
        return Ok(StatusCode::NOT_FOUND.into_response());
    }
    let key = format!("favicon:{domain}");
    let hit = cache().lock().ok().and_then(|mut c| c.get(&key, now));
    if let Some((mime, bytes)) = hit {
        return Ok(crate::handlers::attachment_response(
            "favicon",
            mime,
            bytes.as_ref().clone(),
            CACHE_CONTROL,
        ));
    }
    let taken = bucket()
        .lock()
        .map_err(|_| ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "busy"))?
        .take(Instant::now());
    if let Err(wait) = taken {
        return Ok(too_many(wait));
    }
    let fetcher = shared()?;
    let _permit = permits()
        .acquire()
        .await
        .map_err(|_| ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "busy"))?;
    match fetch_favicon(fetcher, SOURCE, &domain).await {
        Ok((mime, bytes)) => {
            let bytes = Arc::new(bytes);
            if let Ok(mut c) = cache().lock() {
                c.put(key, mime, bytes.clone(), Instant::now());
            }
            Ok(crate::handlers::attachment_response(
                "favicon",
                mime,
                Arc::unwrap_or_clone(bytes),
                CACHE_CONTROL,
            ))
        }
        Err(FaviconError::Miss) => {
            if let Ok(mut m) = misses().lock() {
                m.record(domain, Instant::now());
            }
            Ok(StatusCode::NOT_FOUND.into_response())
        }
        Err(FaviconError::Proxy(e)) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// A 16x16 ICO: header (reserved, type 1, one image) and its directory entry.
    const ICO: &[u8] = &[
        0, 0, 1, 0, 1, 0, 16, 16, 0, 0, 1, 0, 32, 0, 0x68, 4, 0, 0, 22, 0, 0, 0,
    ];
    /// A PNG header declaring 1x1.
    const PNG_1X1: &[u8] = &[
        0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13, b'I', b'H', b'D', b'R', 0, 0,
        0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0,
    ];
    /// A PNG header declaring 32x32.
    const PNG_32: &[u8] = &[
        0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13, b'I', b'H', b'D', b'R', 0, 0,
        0, 32, 0, 0, 0, 32, 8, 6, 0, 0, 0,
    ];

    async fn stub(status: &str, ctype: &str, body: &[u8]) -> String {
        let mut response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        response.extend_from_slice(body);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
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

    async fn fetch_from(
        status: &str,
        ctype: &str,
        body: &[u8],
    ) -> Result<&'static str, FaviconError> {
        let host = stub(status, ctype, body).await;
        let fetcher = ImageFetcher::trusting(&host);
        fetch_favicon(&fetcher, &format!("http://{host}/ip3/"), "shop.example")
            .await
            .map(|(mime, _)| mime)
    }

    #[test]
    fn only_plain_hostnames_are_asked_about() {
        assert_eq!(
            favicon_domain("Shop.Example").as_deref(),
            Some("shop.example")
        );
        assert_eq!(
            favicon_domain(" xn--bcher-kva.example ").as_deref(),
            Some("xn--bcher-kva.example")
        );
        assert_eq!(
            favicon_domain("awsapps.com").as_deref(),
            Some("aws.amazon.com")
        );
        for bad in [
            "",
            "localhost",
            "shop.example:443",
            "shop.example/x",
            "user@shop.example",
            "127.0.0.1",
            "10.0.0.5",
            "[::1]",
            "-shop.example",
            "shop-.example",
            "shop..example",
            "shop.example.",
            "shöp.example",
            "shop_example.example",
            "a.b%2f.example",
        ] {
            assert_eq!(favicon_domain(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn icons_are_sniffed_from_their_header_and_cursors_are_not() {
        assert_eq!(crate::image_proxy::sniff_icon(ICO), Some("image/x-icon"));
        assert_eq!(
            crate::image_proxy::sniff_icon(&[0, 0, 2, 0, 1, 0]),
            None,
            "a cursor"
        );
        assert_eq!(
            crate::image_proxy::sniff_icon(&[0, 0, 1, 0, 0, 0]),
            None,
            "no images"
        );
        assert_eq!(pixel_size("image/x-icon", ICO), Some((16, 16)));
        assert_eq!(pixel_size("image/png", PNG_1X1), Some((1, 1)));
        assert_eq!(pixel_size("image/png", PNG_32), Some((32, 32)));
    }

    #[tokio::test]
    async fn a_real_icon_is_a_logo() {
        assert_eq!(
            fetch_from("200 OK", "image/x-icon", ICO).await,
            Ok("image/x-icon")
        );
        assert_eq!(
            fetch_from("200 OK", "image/png", PNG_32).await,
            Ok("image/png")
        );
    }

    #[tokio::test]
    async fn the_services_fallback_and_non_images_are_misses() {
        // Unknown domains get a 404 (with a placeholder body) from the service.
        assert_eq!(
            fetch_from("404 Not Found", "image/x-icon", ICO).await,
            Err(FaviconError::Miss)
        );
        // A one-pixel image is a placeholder, not a logo.
        assert_eq!(
            fetch_from("200 OK", "image/png", PNG_1X1).await,
            Err(FaviconError::Miss)
        );
        // Markup, however it is labelled, is never served.
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg"><script>x()</script></svg>"#;
        assert_eq!(
            fetch_from("200 OK", "image/x-icon", svg).await,
            Err(FaviconError::Miss)
        );
        assert_eq!(
            fetch_from("200 OK", "text/html", b"<html>nope</html>").await,
            Err(FaviconError::Miss)
        );
    }

    #[test]
    fn a_miss_is_remembered_for_a_while_then_forgotten() {
        let mut m = Misses::default();
        let t0 = Instant::now();
        assert!(!m.is_miss("shop.example", t0));
        m.record("shop.example".into(), t0);
        assert!(m.is_miss("shop.example", t0 + Duration::from_secs(60)));
        assert!(!m.is_miss("shop.example", t0 + MISS_TTL + Duration::from_secs(1)));
    }

    #[test]
    fn the_miss_list_stays_bounded() {
        let mut m = Misses::default();
        let t0 = Instant::now();
        for i in 0..(MISS_CAP + 10) {
            m.record(format!("d{i}.example"), t0);
        }
        assert!(m.at.len() <= MISS_CAP);
    }
}
