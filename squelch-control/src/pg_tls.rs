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
//! | anything else | startup error naming the value | — |
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
/// `sslmode` value itself. The string carries the password.
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
        _ => return Err(PgTlsError::UnsupportedSslMode(mode)),
    };
    let mut config: tokio_postgres::Config = rest.parse()?;
    config.ssl_mode(ssl_mode);
    Ok((config, choice))
}

/// The rustls connector for [`TlsChoice::Verified`]: the RDS bundle as the only
/// roots, ring as the provider (the workspace's one crypto backend), and
/// rustls's standard WebPKI verifier, which checks the chain AND that the
/// certificate names the host tokio-postgres dialed.
pub(crate) fn rds_connector() -> Result<MakeRustlsConnect, PgTlsError> {
    connector_trusting(RDS_CA_BUNDLE)
}

/// [`rds_connector`] with the roots as a parameter, so a test can stand up a
/// server with its own CA and watch the same verifier accept and refuse it.
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

fn decode(s: &str) -> String {
    url::form_urlencoded::parse(format!("x={s}").as_bytes())
        .next()
        .map(|(_, v)| v.into_owned())
        .unwrap_or_default()
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
        for bad in ["prefer", "allow", "verify-ca", "REQUIRE", ""] {
            let s = url(&format!("?sslmode={bad}"));
            let err = parse(&s).unwrap_err();
            let msg = err.to_string();
            assert!(
                matches!(err, PgTlsError::UnsupportedSslMode(ref v) if v == bad),
                "{msg}"
            );
            assert!(msg.contains(&format!("`{bad}`")), "{msg}");
            assert!(!msg.contains(SECRET), "{msg}");
            assert!(!msg.contains("rds.amazonaws.com"), "{msg}");
            assert!(!format!("{err:?}").contains(SECRET));
        }
        let err = parse(&url("?sslmode=require&sslmode=disable")).unwrap_err();
        assert!(matches!(err, PgTlsError::DuplicateSslMode));
        assert!(!err.to_string().contains(SECRET));
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
}
