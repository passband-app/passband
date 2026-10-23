//! TLS for the control store's Postgres connection, chosen by the connection
//! string's own `sslmode`.
//!
//! NO ENV VAR, ON PURPOSE. The URL already says how it wants to be dialed, so the
//! switch rides on it: Railway's private URL carries no `sslmode` and keeps the
//! exact plaintext path it always had, and the AWS one says
//! `sslmode=verify-full` (or `require`) and gets rustls against the RDS CA.
//!
//! | `sslmode` | connector | what is verified |
//! |---|---|---|
//! | absent | `NoTls`, config untouched | nothing: byte-for-byte the pre-TLS path |
//! | `disable` | `NoTls` | nothing |
//! | `require` | rustls, plaintext refused | chain to the RDS bundle AND hostname |
//! | `verify-full` | rustls, plaintext refused | chain to the RDS bundle AND hostname |
//! | anything else | startup error naming the value (if it looks like one) | — |
//!
//! `require` IS STRICTER THAN LIBPQ, deliberately. In libpq `require` encrypts
//! without verifying anything (unless a root cert file happens to exist), which
//! protects against a passive sniffer and nothing else. Here it verifies the
//! chain and the hostname, exactly as `verify-full` does. The divergence fails
//! CLOSED: the only operator it can surprise is one pointing at an IP or a
//! CNAME that the certificate does not name, and that operator gets a loud
//! startup error rather than a silently unauthenticated channel carrying the
//! password. RDS endpoints match their certificates, so on AWS both modes work.
//!
//! `prefer`, `allow` and `verify-ca` are refused rather than approximated:
//! `prefer`/`allow` would fall back to plaintext on a server that declines TLS,
//! which is the downgrade this exists to prevent, and `verify-ca` (chain without
//! hostname) would need a hand-written certificate verifier for a mode nobody
//! here asked for.
//!
//! WHY THIS PARSES `sslmode` ITSELF. tokio-postgres 0.7 knows only
//! `disable`/`prefer`/`require` and rejects `verify-full` as an invalid
//! connection string. It also DEFAULTS to `prefer`, so an absent `sslmode` and
//! an explicit `sslmode=prefer` come out of its parser identically. Both
//! problems need the raw parameter, so it is read here first, mirroring
//! tokio-postgres's own two grammars (URL and `key=value`), and removed from
//! the string handed on; the mode is then set on the parsed config directly.

use std::sync::Arc;

use rustls::pki_types::CertificateDer;
use rustls::pki_types::pem::PemObject;
use tokio_postgres::config::SslMode;
use tokio_postgres_rustls::MakeRustlsConnect;

/// The AWS RDS global CA bundle: every RDS region's root and intermediate CAs.
///
/// SOURCE: https://truststore.pki.rds.amazonaws.com/global/global-bundle.pem
/// FETCHED: 2026-10-23 (114 certificates, sha256
/// 90a6f1a7978a1b1b096dca6c57511ca48d3b8cf052f8e704baf358f34ea2f21b).
///
/// Baked in rather than read from disk so the image cannot ship without it and
/// so no env var is needed to find it. AWS rotates RDS CAs on a years-long
/// schedule; when it does, re-fetch from the URL above and update the date.
const RDS_CA_BUNDLE: &[u8] = include_bytes!("../certs/rds-global-bundle.pem");

/// Which connector the URL asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TlsChoice {
    /// `NoTls`: no `sslmode`, or `sslmode=disable`.
    Plain,
    /// rustls with the RDS bundle, hostname verified, plaintext refused.
    Verified,
}

/// Why a connection string could not be turned into a config.
///
/// Carries NO PART OF THE STRING except, for [`Self::UnsupportedSslMode`], the
/// `sslmode` value itself, and only when it looks like an sslmode
/// (`^[a-z-]{1,16}$`; see [`shown_mode`]). The string carries the password.
#[derive(Debug, thiserror::Error)]
pub enum PgTlsError {
    #[error(
        "control store: unsupported sslmode `{0}` in the database URL \
         (use disable, require or verify-full)"
    )]
    UnsupportedSslMode(String),
    #[error("control store: sslmode is given more than once in the database URL")]
    DuplicateSslMode,
    /// tokio-postgres's own parse error, which is `invalid connection string`
    /// and likewise carries none of it.
    #[error("control store: {0}")]
    Parse(#[from] tokio_postgres::Error),
    #[error("control store: the built-in RDS CA bundle would not load: {0}")]
    Bundle(String),
}

/// Parse `database_url` and decide its connector.
///
/// With no `sslmode` the string goes to tokio-postgres UNTOUCHED, which is what
/// keeps today's Railway URL on exactly today's path.
pub(crate) fn parse(database_url: &str) -> Result<(tokio_postgres::Config, TlsChoice), PgTlsError> {
    let Some((mode, rest)) = split_sslmode(database_url)? else {
        return Ok((database_url.parse()?, TlsChoice::Plain));
    };
    let (ssl_mode, choice) = match mode.as_str() {
        "disable" => (SslMode::Disable, TlsChoice::Plain),
        "require" | "verify-full" => (SslMode::Require, TlsChoice::Verified),
        _ => return Err(PgTlsError::UnsupportedSslMode(shown_mode(&mode))),
    };
    let mut config: tokio_postgres::Config = rest.parse()?;
    config.ssl_mode(ssl_mode);
    Ok((config, choice))
}

/// What an [`PgTlsError::UnsupportedSslMode`] may echo: the value if it could
/// only be a mistyped mode, a placeholder otherwise. A value is not always
/// just the value: in `key=value` form a backslash-escaped space joins the
/// next parameter on, so `sslmode=require\ password=hunter2` reads as ONE
/// sslmode whose value contains the password.
const UNPRINTABLE_MODE: &str = "<not shown>";

fn shown_mode(mode: &str) -> String {
    let looks_like_a_mode =
        (1..=16).contains(&mode.len()) && mode.bytes().all(|c| c.is_ascii_lowercase() || c == b'-');
    if looks_like_a_mode {
        mode.to_string()
    } else {
        UNPRINTABLE_MODE.to_string()
    }
}

/// The rustls connector for [`TlsChoice::Verified`]: the RDS bundle as the only
/// roots, ring as the provider (the workspace's one crypto backend), and
/// rustls's standard WebPKI verifier, which checks the chain AND that the
/// certificate names the host tokio-postgres dialed.
pub(crate) fn rds_connector() -> Result<MakeRustlsConnect, PgTlsError> {
    connector_trusting(RDS_CA_BUNDLE)
}

/// [`rds_connector`] with the roots as a parameter, so a test can stand up a
/// server with its own CA and watch the same verifier accept and refuse it
/// (`tests::the_verifier_checks_chain_and_hostname`).
fn connector_trusting(pem: &[u8]) -> Result<MakeRustlsConnect, PgTlsError> {
    let mut roots = rustls::RootCertStore::empty();
    for cert in CertificateDer::pem_slice_iter(pem) {
        let cert = cert.map_err(|e| PgTlsError::Bundle(e.to_string()))?;
        roots
            .add(cert)
            .map_err(|e| PgTlsError::Bundle(e.to_string()))?;
    }
    if roots.is_empty() {
        return Err(PgTlsError::Bundle("no certificates".into()));
    }
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|e| PgTlsError::Bundle(e.to_string()))?
    .with_root_certificates(roots)
    .with_no_client_auth();
    Ok(MakeRustlsConnect::new(config))
}

/// Find `sslmode` in either connection-string grammar and return its value
/// with the string minus that parameter, or `None` when it is absent.
fn split_sslmode(s: &str) -> Result<Option<(String, String)>, PgTlsError> {
    for prefix in ["postgresql://", "postgres://"] {
        if s.starts_with(prefix) {
            return split_url(s);
        }
    }
    split_key_value(s)
}

/// URL form. Mirrors tokio-postgres's `UrlParser`: credentials run to the FIRST
/// `@` anywhere, and the query starts at the first `?` after them.
fn split_url(s: &str) -> Result<Option<(String, String)>, PgTlsError> {
    let after_creds = s.find('@').map_or(0, |at| at + 1);
    let Some(q) = s[after_creds..].find('?').map(|i| after_creds + i) else {
        return Ok(None);
    };
    let (head, query) = (&s[..q], &s[q + 1..]);
    let mut mode = None;
    let mut kept = Vec::new();
    for pair in query.split('&') {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        if decode(key) == "sslmode" {
            if mode.replace(decode(value)).is_some() {
                return Err(PgTlsError::DuplicateSslMode);
            }
        } else {
            kept.push(pair);
        }
    }
    let Some(mode) = mode else {
        return Ok(None);
    };
    let rest = if kept.is_empty() {
        head.to_string()
    } else {
        format!("{head}?{}", kept.join("&"))
    };
    Ok(Some((mode, rest)))
}

/// Percent-decoding exactly as tokio-postgres's URL parser does it (`+` stays a
/// `+`; it is not form encoding). Lossy where tokio-postgres would error on
/// invalid UTF-8: such a key is not `sslmode`, and such a value is refused.
fn decode(s: &str) -> String {
    percent_encoding::percent_decode_str(s)
        .decode_utf8_lossy()
        .into_owned()
}

/// `key=value` form. Mirrors tokio-postgres's `Parser`: whitespace-separated
/// parameters, optional whitespace around `=`, values either bare or
/// single-quoted, backslash escaping the next character in both. A string this
/// cannot tokenize is passed through untouched so tokio-postgres reports it in
/// its own (string-free) words.
fn split_key_value(s: &str) -> Result<Option<(String, String)>, PgTlsError> {
    let b = s.as_bytes();
    let mut i = 0;
    let mut mode = None;
    let mut span = (0, 0);
    let ws = |c: u8| c.is_ascii_whitespace();
    loop {
        while i < b.len() && ws(b[i]) {
            i += 1;
        }
        if i == b.len() {
            break;
        }
        let start = i;
        while i < b.len() && !ws(b[i]) && b[i] != b'=' {
            i += 1;
        }
        let key = &s[start..i];
        while i < b.len() && ws(b[i]) {
            i += 1;
        }
        if i == b.len() || b[i] != b'=' || key.is_empty() {
            return Ok(None);
        }
        i += 1;
        while i < b.len() && ws(b[i]) {
            i += 1;
        }
        let mut value = Vec::new();
        if i < b.len() && b[i] == b'\'' {
            i += 1;
            loop {
                match b.get(i) {
                    None => return Ok(None),
                    Some(b'\'') => {
                        i += 1;
                        break;
                    }
                    Some(b'\\') => {
                        if let Some(&c) = b.get(i + 1) {
                            value.push(c);
                        }
                        i += 2;
                    }
                    Some(&c) => {
                        value.push(c);
                        i += 1;
                    }
                }
            }
        } else {
            while i < b.len() && !ws(b[i]) {
                if b[i] == b'\\' {
                    if let Some(&c) = b.get(i + 1) {
                        value.push(c);
                    }
                    i += 2;
                } else {
                    value.push(b[i]);
                    i += 1;
                }
            }
        }
        let i_end = i.min(b.len());
        i = i_end;
        if key == "sslmode" {
            let v = String::from_utf8_lossy(&value).into_owned();
            if mode.replace(v).is_some() {
                return Err(PgTlsError::DuplicateSslMode);
            }
            span = (start, i_end);
        }
    }
    Ok(mode.map(|m| (m, format!("{} {}", &s[..span.0], &s[span.1..]))))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "hunter2-very-secret";

    fn url(query: &str) -> String {
        format!(
            "postgres://ctl:{SECRET}@db.example.us-east-1.rds.amazonaws.com:5432/control{query}"
        )
    }

    fn choice(s: &str) -> (SslMode, TlsChoice) {
        let (config, choice) = parse(s).unwrap();
        (config.get_ssl_mode(), choice)
    }

    #[test]
    fn absent_sslmode_is_todays_plain_path_untouched() {
        // Today's Railway URL shape: no sslmode at all.
        let railway =
            format!("postgres://postgres:{SECRET}@postgres.railway.internal:5432/railway");
        let (config, c) = parse(&railway).unwrap();
        assert_eq!(c, TlsChoice::Plain);
        // tokio-postgres's own default, i.e. what `parse()` alone produced
        // before this module existed.
        let before: tokio_postgres::Config = railway.parse().unwrap();
        assert_eq!(format!("{config:?}"), format!("{before:?}"));
        assert_eq!(config.get_ssl_mode(), SslMode::Prefer);
        // Other query params, still no sslmode.
        assert_eq!(
            choice(&url("?options=-csearch_path%3Dfoo")),
            (SslMode::Prefer, TlsChoice::Plain)
        );
    }

    #[test]
    fn disable_is_plain() {
        assert_eq!(
            choice(&url("?sslmode=disable")),
            (SslMode::Disable, TlsChoice::Plain)
        );
    }

    #[test]
    fn require_and_verify_full_are_verified_tls_with_no_fallback() {
        for q in [
            "?sslmode=require",
            "?sslmode=verify-full",
            "?application_name=a&sslmode=verify-full&connect_timeout=5",
        ] {
            assert_eq!(
                choice(&url(q)),
                (SslMode::Require, TlsChoice::Verified),
                "{q}"
            );
        }
    }

    #[test]
    fn stripping_sslmode_keeps_every_other_parameter() {
        let (config, _) = parse(&url(
            "?options=-csearch_path%3Dfoo&sslmode=verify-full&application_name=x",
        ))
        .unwrap();
        assert_eq!(config.get_options(), Some("-csearch_path=foo"));
        assert_eq!(config.get_application_name(), Some("x"));
        assert_eq!(config.get_password(), Some(SECRET.as_bytes()));
        assert_eq!(config.get_dbname(), Some("control"));
    }

    #[test]
    fn key_value_form_is_read_too() {
        let kv = |m: &str| format!("host=h user=u password='a b\\'c' {m} dbname=d");
        assert_eq!(choice(&kv("")), (SslMode::Prefer, TlsChoice::Plain));
        assert_eq!(
            choice(&kv("sslmode=disable")),
            (SslMode::Disable, TlsChoice::Plain)
        );
        assert_eq!(
            choice(&kv("sslmode = verify-full")),
            (SslMode::Require, TlsChoice::Verified)
        );
        let (config, _) = parse(&kv("sslmode=verify-full")).unwrap();
        assert_eq!(config.get_password(), Some("a b'c".as_bytes()));
        assert_eq!(config.get_dbname(), Some("d"));
        // An sslmode hidden INSIDE a quoted password is not an sslmode.
        assert_eq!(
            choice("host=h password='x sslmode=require' dbname=d"),
            (SslMode::Prefer, TlsChoice::Plain)
        );
    }

    #[test]
    fn other_modes_are_refused_naming_the_value_and_never_the_url() {
        for (bad, shown) in [
            ("prefer", "prefer"),
            ("allow", "allow"),
            ("verify-ca", "verify-ca"),
            ("REQUIRE", UNPRINTABLE_MODE),
            ("", UNPRINTABLE_MODE),
            ("verify-full-please-x", UNPRINTABLE_MODE),
        ] {
            let s = url(&format!("?sslmode={bad}"));
            let err = parse(&s).unwrap_err();
            let msg = err.to_string();
            assert!(
                matches!(err, PgTlsError::UnsupportedSslMode(ref v) if v == shown),
                "{msg}"
            );
            assert!(msg.contains(&format!("`{shown}`")), "{msg}");
            assert!(!msg.contains(SECRET), "{msg}");
            assert!(!msg.contains("rds.amazonaws.com"), "{msg}");
            assert!(!format!("{err:?}").contains(SECRET));
        }
        let err = parse(&url("?sslmode=require&sslmode=disable")).unwrap_err();
        assert!(matches!(err, PgTlsError::DuplicateSslMode));
        assert!(!err.to_string().contains(SECRET));
    }

    /// Review finding: an escaped space glues the next parameter onto the
    /// sslmode value, so echoing the value would echo the password.
    #[test]
    fn an_sslmode_that_swallowed_the_password_is_not_echoed() {
        for s in [
            "host=h user=u sslmode=require\\ password=hunter2 dbname=d",
            "host=h user=u sslmode='require password=hunter2' dbname=d",
        ] {
            let err = parse(s).unwrap_err();
            assert!(matches!(err, PgTlsError::UnsupportedSslMode(_)), "{err}");
            assert!(!err.to_string().contains("hunter2"), "{err}");
            assert!(!format!("{err:?}").contains("hunter2"), "{err:?}");
        }
        // And in a URL, where `%20` decodes to the same thing.
        let err = parse(&url("?sslmode=require%20password%3Dhunter2")).unwrap_err();
        assert!(!err.to_string().contains("hunter2"), "{err}");
    }

    #[test]
    fn url_values_are_percent_decoded_like_tokio_postgres() {
        assert_eq!(decode("verify%2Dfull"), "verify-full");
        // `+` is literal in tokio-postgres's parser, not a space.
        assert_eq!(decode("a+b"), "a+b");
        assert_eq!(
            choice(&url("?sslmode=verify%2dfull")),
            (SslMode::Require, TlsChoice::Verified)
        );
    }

    #[test]
    fn a_malformed_url_still_carries_no_secret() {
        let err = parse(&format!(
            "postgres://u:{SECRET}@h:notaport/db?sslmode=require"
        ))
        .unwrap_err();
        assert!(!err.to_string().contains(SECRET), "{err}");
    }

    #[test]
    fn the_rds_bundle_loads() {
        rds_connector().unwrap();
        let n = CertificateDer::pem_slice_iter(RDS_CA_BUNDLE).count();
        assert!(n > 100, "{n}");
    }

    /// The verifier itself, with no database: a throwaway CA signs a leaf
    /// for `localhost`, and a loopback server answers the SSLRequest with `S`
    /// and then speaks TLS with that leaf. The connector built by
    /// [`connector_trusting`] (the one [`rds_connector`] wraps) must accept
    /// it under the name it names, and refuse it under another name or when
    /// trusting a different CA. This is the whole point of verify-full, so it
    /// is asserted on the wire, not inferred from configuration.
    mod verifier {
        use super::super::*;
        use std::net::{IpAddr, Ipv4Addr};
        use std::time::Duration;

        use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, Issuer, KeyPair};
        use rustls::pki_types::PrivateKeyDer;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        struct Pki {
            ca_pem: String,
            leaf: CertificateDer<'static>,
            leaf_key: Vec<u8>,
        }

        fn pki(ca_name: &str, leaf_names: &[&str]) -> Pki {
            let ca_key = KeyPair::generate().unwrap();
            let mut ca = CertificateParams::new(Vec::<String>::new()).unwrap();
            ca.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
            ca.distinguished_name.push(DnType::CommonName, ca_name);
            let ca_cert = ca.self_signed(&ca_key).unwrap();
            let issuer = Issuer::new(ca, ca_key);
            let leaf_key = KeyPair::generate().unwrap();
            let names: Vec<String> = leaf_names.iter().map(|n| n.to_string()).collect();
            let leaf = CertificateParams::new(names)
                .unwrap()
                .signed_by(&leaf_key, &issuer)
                .unwrap();
            Pki {
                ca_pem: ca_cert.pem(),
                leaf: leaf.der().clone(),
                leaf_key: leaf_key.serialize_der(),
            }
        }

        /// One connection: SSLRequest -> `S` -> TLS handshake. Resolves to
        /// whether the SERVER saw the handshake complete.
        async fn serve_once(pki: &Pki) -> (u16, tokio::task::JoinHandle<bool>) {
            let config = rustls::ServerConfig::builder_with_provider(Arc::new(
                rustls::crypto::ring::default_provider(),
            ))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![pki.leaf.clone()],
                PrivateKeyDer::Pkcs8(pki.leaf_key.clone().into()),
            )
            .unwrap();
            let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
            let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
                .await
                .unwrap();
            let port = listener.local_addr().unwrap().port();
            let server = tokio::spawn(async move {
                let (mut sock, _) = listener.accept().await.unwrap();
                let mut head = [0u8; 8];
                sock.read_exact(&mut head).await.unwrap();
                assert_eq!(
                    head,
                    [0, 0, 0, 8, 0x04, 0xd2, 0x16, 0x2f],
                    "not an SSLRequest"
                );
                sock.write_all(b"S").await.unwrap();
                let wait = Duration::from_secs(10);
                matches!(
                    tokio::time::timeout(wait, acceptor.accept(sock)).await,
                    Ok(Ok(_))
                )
            });
            (port, server)
        }

        /// Dial `host` (always at 127.0.0.1) with sslmode=require semantics
        /// and the connector trusting `ca_pem`; the rustls error, if any.
        async fn dial(host: &str, port: u16, ca_pem: &str) -> Option<rustls::Error> {
            let mut config = tokio_postgres::Config::new();
            config
                .host(host)
                .hostaddr(IpAddr::V4(Ipv4Addr::LOCALHOST))
                .port(port)
                .user("u")
                .dbname("d")
                .ssl_mode(SslMode::Require)
                .connect_timeout(Duration::from_secs(10));
            let connector = connector_trusting(ca_pem.as_bytes()).unwrap();
            // Never Ok: the server hangs up after the handshake, before any
            // Postgres protocol. What matters is WHY it fails.
            let err = config.connect(connector).await.err().expect("connected?");
            rustls_cause(&err)
        }

        fn rustls_cause(err: &(dyn std::error::Error + 'static)) -> Option<rustls::Error> {
            let mut cur = Some(err);
            while let Some(e) = cur {
                if let Some(r) = e.downcast_ref::<rustls::Error>() {
                    return Some(r.clone());
                }
                // io::Error's source() skips its own payload; look inside.
                if let Some(r) = e
                    .downcast_ref::<std::io::Error>()
                    .and_then(|io| io.get_ref())
                    .and_then(|inner| inner.downcast_ref::<rustls::Error>())
                {
                    return Some(r.clone());
                }
                cur = e.source();
            }
            None
        }

        fn is_not_valid_for_name(e: &Option<rustls::Error>) -> bool {
            use rustls::CertificateError as C;
            matches!(
                e,
                Some(rustls::Error::InvalidCertificate(
                    C::NotValidForName | C::NotValidForNameContext { .. }
                ))
            )
        }

        #[tokio::test]
        async fn the_verifier_checks_chain_and_hostname() {
            let good = pki("pg_tls test CA", &["localhost"]);

            // The name the certificate carries, chained to the trusted CA.
            let (port, server) = serve_once(&good).await;
            let err = dial("localhost", port, &good.ca_pem).await;
            assert!(err.is_none(), "localhost refused: {err:?}");
            assert!(
                server.await.unwrap(),
                "server never completed the handshake"
            );

            // Same server, same CA, dialed by a name the certificate lacks.
            let (port, server) = serve_once(&good).await;
            let err = dial("127.0.0.1", port, &good.ca_pem).await;
            assert!(is_not_valid_for_name(&err), "127.0.0.1: {err:?}");
            assert!(!server.await.unwrap());

            // Right name, but a CA the connector does not trust. (A distinct
            // CA name: with the same name webpki would match it as the issuer
            // and fail on the signature instead, BadSignature, still a refusal.)
            let stranger = pki("some other CA", &["localhost"]);
            let (port, server) = serve_once(&good).await;
            let err = dial("localhost", port, &stranger.ca_pem).await;
            assert!(
                matches!(
                    err,
                    Some(rustls::Error::InvalidCertificate(
                        rustls::CertificateError::UnknownIssuer
                    ))
                ),
                "other CA: {err:?}"
            );
            assert!(!server.await.unwrap());
        }
    }
}
