//! The ONE outbound HTTP client that fetches a URL a stranger chose.
//!
//! An unsubscribe link is written by whoever sent the mail, and the automatic
//! unsubscribe (`crate::unsub_job`) fetches it from inside the daemon. Without a
//! guard that is a server-side request forgery primitive aimed at whatever the
//! daemon can reach: on a self-host that includes the daemon's own human door
//! on loopback, the user's LAN, and a cloud metadata endpoint. Hosted tenants
//! have a NetworkPolicy under them (docs/SECURITY.md), but a self-host has
//! nothing, so the guard lives here, in code, for every deployment:
//!
//!   - http and https only, ports 80 and 443 only, no userinfo;
//!   - every name resolves through [`PublicOnly`], which drops loopback,
//!     private, link-local, CGNAT, multicast and reserved addresses. reqwest
//!     connects to exactly the addresses the resolver returned, so a name that
//!     answers public for the check and private for the connect cannot exist;
//!   - an IP LITERAL never reaches a resolver, so [`check_url`] judges it;
//!   - redirects are followed BY HAND, so every hop passes the same two checks;
//!   - bodies are capped, every request is timed out, and the cookie jar lives
//!     exactly as long as one job.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::{Method, StatusCode};
use url::Url;

/// Hops a single fetch may follow. Click-tracking redirects chain two or three
/// deep; five is room for that and not for a loop.
const MAX_REDIRECTS: usize = 5;
/// Body cap. An unsubscribe page that needs more than this to say "you're
/// done" is not one we can read anyway.
pub(crate) const MAX_BODY_BYTES: usize = 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(12);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(6);

/// Honest about what it is. Bot walls that refuse it hand the link to the user,
/// which is the right outcome for a page that wants a person.
const USER_AGENT: &str = "Mozilla/5.0 (compatible; Passband unsubscribe; +https://passband.app)";

#[derive(Debug, thiserror::Error)]
pub(crate) enum FetchError {
    /// The URL, or a hop of it, points somewhere this client will not go.
    #[error("refused: {0}")]
    Refused(&'static str),
    #[error("too many redirects")]
    TooManyRedirects,
    /// DNS, connect, TLS, timeout: the site could not be reached from here.
    #[error("transport: {0}")]
    Transport(String),
}

/// What came back, after any redirects.
#[derive(Debug)]
pub(crate) struct Fetched {
    pub status: StatusCode,
    pub final_url: Url,
    pub content_type: Option<String>,
    pub body: String,
}

impl Fetched {
    pub fn is_html(&self) -> bool {
        self.content_type
            .as_deref()
            .map(|c| {
                let c = c.to_ascii_lowercase();
                c.starts_with("text/html") || c.starts_with("application/xhtml")
            })
            // No header at all: sniff rather than refuse.
            .unwrap_or_else(|| self.body.trim_start().starts_with('<'))
    }
}

/// One job's client. Build a fresh one per job: the cookie jar is the job's
/// session with the sender's site and must not outlive it.
pub(crate) struct Fetcher {
    client: reqwest::Client,
    allow_local: bool,
}

impl Fetcher {
    /// `allow_local` exists for tests that serve pages from loopback. Production
    /// passes `false`; nothing reads it from config.
    pub fn new(allow_local: bool) -> Result<Self, FetchError> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .dns_resolver(Arc::new(PublicOnly { allow_local }))
            .cookie_store(true)
            .no_proxy()
            .timeout(REQUEST_TIMEOUT)
            .connect_timeout(CONNECT_TIMEOUT)
            .user_agent(USER_AGENT)
            .build()
            .map_err(|e| FetchError::Transport(redact(&e)))?;
        Ok(Self {
            client,
            allow_local,
        })
    }

    /// RFC 8058: one POST of `List-Unsubscribe=One-Click` to the header's URL,
    /// redirects NOT followed. Returns the status; the caller decides what a
    /// non-2xx means. The jar is empty at this point (it is the job's first
    /// request), which is what the RFC asks: no cookies, no context.
    pub async fn one_click(&self, url: &str) -> Result<StatusCode, FetchError> {
        let url = parse(url)?;
        check_url(&url, self.allow_local)?;
        let resp = self
            .send_hop(&Method::POST, &url, Some("List-Unsubscribe=One-Click"))
            .await?;
        Ok(resp.status())
    }

    pub async fn get(&self, url: &str) -> Result<Fetched, FetchError> {
        self.follow(Method::GET, parse(url)?, None).await
    }

    /// Submit a form the way a browser would: GET puts the pairs in the query,
    /// POST sends them url-encoded.
    pub async fn submit(
        &self,
        method: Method,
        action: Url,
        pairs: &[(String, String)],
    ) -> Result<Fetched, FetchError> {
        if method == Method::GET {
            let mut url = action;
            url.query_pairs_mut()
                .clear()
                .extend_pairs(pairs.iter().map(|(k, v)| (k.as_str(), v.as_str())));
            self.follow(Method::GET, url, None).await
        } else {
            let body = url::form_urlencoded::Serializer::new(String::new())
                .extend_pairs(pairs.iter().map(|(k, v)| (k.as_str(), v.as_str())))
                .finish();
            self.follow(Method::POST, action, Some(body)).await
        }
    }

    async fn follow(
        &self,
        mut method: Method,
        mut url: Url,
        mut body: Option<String>,
    ) -> Result<Fetched, FetchError> {
        for _ in 0..=MAX_REDIRECTS {
            check_url(&url, self.allow_local)?;
            let mut resp = self.send_hop(&method, &url, body.as_deref()).await?;
            let status = resp.status();
            if status.is_redirection()
                && let Some(loc) = resp.headers().get(reqwest::header::LOCATION)
            {
                let loc = loc
                    .to_str()
                    .map_err(|_| FetchError::Refused("bad redirect"))?;
                url = url
                    .join(loc)
                    .map_err(|_| FetchError::Refused("bad redirect"))?;
                // 307/308 keep the method and body; every other redirect is a
                // GET from here on, which is what browsers do.
                if !matches!(
                    status,
                    StatusCode::TEMPORARY_REDIRECT | StatusCode::PERMANENT_REDIRECT
                ) {
                    method = Method::GET;
                    body = None;
                }
                continue;
            }
            let content_type = resp
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            let final_url = resp.url().clone();
            let mut bytes = Vec::new();
            while let Some(chunk) = resp
                .chunk()
                .await
                .map_err(|e| FetchError::Transport(redact(&e)))?
            {
                let room = MAX_BODY_BYTES - bytes.len();
                if chunk.len() >= room {
                    bytes.extend_from_slice(&chunk[..room]);
                    break;
                }
                bytes.extend_from_slice(&chunk);
            }
            return Ok(Fetched {
                status,
                final_url,
                content_type,
                body: String::from_utf8_lossy(&bytes).into_owned(),
            });
        }
        Err(FetchError::TooManyRedirects)
    }
}

impl Fetcher {
    /// One request, HTTPS FIRST. Hosted tenants may only leave on 443, and an
    /// `http://` unsubscribe link is common enough (older ESPs, tracking
    /// domains) that refusing it outright would send a lot of easy cases to the
    /// user's browser. So an http URL is tried as https first, and only a
    /// transport failure there (no TLS on that host) falls back to the URL as
    /// written. Loopback test servers speak plain http, so tests skip this.
    async fn send_hop(
        &self,
        method: &Method,
        url: &Url,
        body: Option<&str>,
    ) -> Result<reqwest::Response, FetchError> {
        if url.scheme() == "http" && !self.allow_local {
            let mut secure = url.clone();
            if secure.set_scheme("https").is_ok()
                && secure.set_port(None).is_ok()
                && let Ok(resp) = self.send_raw(method, &secure, body).await
            {
                return Ok(resp);
            }
        }
        self.send_raw(method, url, body).await
    }

    async fn send_raw(
        &self,
        method: &Method,
        url: &Url,
        body: Option<&str>,
    ) -> Result<reqwest::Response, FetchError> {
        let mut req = self
            .client
            .request(method.clone(), url.clone())
            .header(reqwest::header::ACCEPT, "text/html,application/xhtml+xml");
        if let Some(b) = body {
            req = req
                .header(
                    reqwest::header::CONTENT_TYPE,
                    "application/x-www-form-urlencoded",
                )
                .body(b.to_string());
        }
        req.send()
            .await
            .map_err(|e| FetchError::Transport(redact(&e)))
    }
}

fn parse(url: &str) -> Result<Url, FetchError> {
    Url::parse(url.trim()).map_err(|_| FetchError::Refused("not a url"))
}

/// The per-hop URL check. The resolver covers names; this covers everything a
/// resolver never sees.
pub(crate) fn check_url(url: &Url, allow_local: bool) -> Result<(), FetchError> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err(FetchError::Refused("scheme"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(FetchError::Refused("userinfo"));
    }
    if allow_local {
        return Ok(());
    }
    if !matches!(url.port(), None | Some(80) | Some(443)) {
        return Err(FetchError::Refused("port"));
    }
    match url.host() {
        Some(url::Host::Ipv4(ip)) if !is_public(IpAddr::V4(ip)) => {
            Err(FetchError::Refused("address"))
        }
        Some(url::Host::Ipv6(ip)) if !is_public(IpAddr::V6(ip)) => {
            Err(FetchError::Refused("address"))
        }
        Some(url::Host::Domain(d)) => {
            let d = d.trim_end_matches('.').to_ascii_lowercase();
            // Names that only ever mean "this machine" or "this network". The
            // resolver would catch most of them anyway; this skips the lookup.
            let local = d == "localhost"
                || [".localhost", ".local", ".internal", ".home.arpa", ".lan"]
                    .iter()
                    .any(|s| d.ends_with(s));
            if local || !d.contains('.') {
                Err(FetchError::Refused("address"))
            } else {
                Ok(())
            }
        }
        None => Err(FetchError::Refused("no host")),
        _ => Ok(()),
    }
}

/// Is this an address on the public internet?
pub(crate) fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => is_public_v6(v6),
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_unspecified()
        || ip.is_multicast()
        || a == 0
        // CGNAT, 100.64.0.0/10.
        || (a == 100 && (64..128).contains(&b))
        // IETF protocol assignments, 192.0.0.0/24.
        || (a == 192 && b == 0 && c == 0)
        // Benchmarking, 198.18.0.0/15.
        || (a == 198 && (b == 18 || b == 19))
        // Reserved, 240.0.0.0/4.
        || a >= 240)
}

fn is_public_v6(ip: Ipv6Addr) -> bool {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_public_v4(v4);
    }
    let s = ip.segments();
    // ::, ::1, and the deprecated IPv4-compatible block ::a.b.c.d.
    if s[..6].iter().all(|x| *x == 0) {
        return false;
    }
    // NAT64 (64:ff9b::/96) and 6to4 (2002::/16) carry a v4 address inside.
    if s[0] == 0x64 && s[1] == 0xff9b && s[2..6].iter().all(|x| *x == 0) {
        return is_public_v4(Ipv4Addr::new(
            (s[6] >> 8) as u8,
            s[6] as u8,
            (s[7] >> 8) as u8,
            s[7] as u8,
        ));
    }
    if s[0] == 0x2002 {
        return is_public_v4(Ipv4Addr::new(
            (s[1] >> 8) as u8,
            s[1] as u8,
            (s[2] >> 8) as u8,
            s[2] as u8,
        ));
    }
    !(ip.is_multicast()
        // Unique local, fc00::/7.
        || (s[0] & 0xfe00) == 0xfc00
        // Link local, fe80::/10.
        || (s[0] & 0xffc0) == 0xfe80
        // Site local (deprecated), fec0::/10.
        || (s[0] & 0xffc0) == 0xfec0
        // Documentation, 2001:db8::/32.
        || (s[0] == 0x2001 && s[1] == 0x0db8))
}

/// Resolves a name and keeps only its public addresses. No public address at
/// all is an error, so the connect never happens.
struct PublicOnly {
    allow_local: bool,
}

impl Resolve for PublicOnly {
    fn resolve(&self, name: Name) -> Resolving {
        let allow_local = self.allow_local;
        Box::pin(async move {
            let host = name.as_str().to_string();
            let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0))
                .await?
                .filter(|a| allow_local || is_public(a.ip()))
                .collect();
            if addrs.is_empty() {
                let err: Box<dyn std::error::Error + Send + Sync> = Box::new(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "name has no public address",
                ));
                return Err(err);
            }
            let addrs: Addrs = Box::new(addrs.into_iter());
            Ok(addrs)
        })
    }
}

/// A transport error's kind, WITHOUT its URL: the URL carries the sender's
/// per-recipient token, and these strings reach logs.
fn redact(e: &reqwest::Error) -> String {
    let kind = if e.is_timeout() {
        "timeout"
    } else if e.is_connect() {
        "connect"
    } else if e.is_body() || e.is_decode() {
        "body"
    } else if e.is_request() {
        "request"
    } else {
        "other"
    };
    kind.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refused(url: &str) -> bool {
        matches!(
            check_url(&Url::parse(url).unwrap(), false),
            Err(FetchError::Refused(_))
        )
    }

    #[test]
    fn the_daemon_itself_and_its_neighbours_are_refused() {
        for url in [
            "http://127.0.0.1:8848/client/stats",
            "http://127.0.0.1/",
            "http://localhost/",
            "http://api.localhost/",
            "http://[::1]/",
            "http://10.0.0.1/",
            "http://172.16.4.2/",
            "http://192.168.1.1/",
            "http://169.254.169.254/latest/meta-data/",
            "http://100.64.0.1/",
            "http://0.0.0.0/",
            "http://[::ffff:127.0.0.1]/",
            "http://[fd00::1]/",
            "http://[fe80::1]/",
            "http://[64:ff9b::a9fe:a9fe]/",
            "http://[2002:7f00:1::]/",
            "http://router.lan/",
            "http://printer.local/",
            "http://metadata.google.internal/",
            "http://intranet/",
        ] {
            assert!(refused(url), "{url} must be refused");
        }
    }

    #[test]
    fn odd_ports_schemes_and_userinfo_are_refused() {
        for url in [
            "https://example.com:8443/u",
            "http://example.com:22/",
            "ftp://example.com/u",
            "file:///etc/passwd",
            "https://user:pw@example.com/u",
        ] {
            assert!(refused(url), "{url} must be refused");
        }
    }

    #[test]
    fn ordinary_unsubscribe_links_pass() {
        for url in [
            "https://click.eu.marketing.example.com/ls/click?upn=abc",
            "http://list-manage.example.com/unsubscribe?u=1&id=2",
            "https://example.com:443/u",
            "https://93.184.216.34/u",
            "https://[2606:2800:220:1:248:1893:25c8:1946]/u",
        ] {
            assert!(
                check_url(&Url::parse(url).unwrap(), false).is_ok(),
                "{url} must pass"
            );
        }
    }

    #[test]
    fn public_addresses_are_public() {
        assert!(is_public("8.8.8.8".parse().unwrap()));
        assert!(is_public("2606:4700::1111".parse().unwrap()));
        assert!(!is_public("::".parse().unwrap()));
        assert!(!is_public("224.0.0.1".parse().unwrap()));
        assert!(!is_public("198.18.0.5".parse().unwrap()));
    }

    /// The resolver path: a NAME that resolves to loopback never connects.
    #[tokio::test]
    async fn a_name_resolving_to_loopback_never_connects() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let accepted = tokio::spawn(async move {
            tokio::time::timeout(Duration::from_millis(800), listener.accept())
                .await
                .is_ok()
        });
        // `localhost` is caught by name; `localtest.me`-style names need the
        // network, so drive the resolver directly with a name the system
        // resolves to loopback.
        let r = PublicOnly { allow_local: false };
        let out = r.resolve("localhost".parse::<Name>().unwrap()).await;
        assert!(out.is_err(), "loopback answers are dropped");
        // And the fetcher refuses the hop outright.
        let f = Fetcher::new(false).unwrap();
        let err = f
            .get(&format!("http://127.0.0.1:{port}/"))
            .await
            .unwrap_err();
        assert!(matches!(err, FetchError::Refused(_)));
        assert!(!accepted.await.unwrap(), "no connection was ever made");
    }
}
