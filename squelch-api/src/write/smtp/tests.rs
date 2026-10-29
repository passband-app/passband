use super::*;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::TcpListener;

fn s(b: &[u8]) -> &str {
    std::str::from_utf8(b).unwrap()
}

// ---- Bcc stripping -----------------------------------------------------------

#[test]
fn bcc_comes_off_the_transmitted_copy_and_nothing_else_moves() {
    let raw = b"To: a@x.test\r\nBcc: hidden@y.test\r\nSubject: hi\r\n\r\nbody\r\n";
    assert_eq!(
        s(&transmitted_copy(raw)),
        "To: a@x.test\r\nSubject: hi\r\n\r\nbody\r\n"
    );
}

#[test]
fn a_folded_bcc_comes_off_with_every_continuation_line() {
    let raw = b"To: a@x.test\r\nBcc: one@y.test,\r\n two@y.test,\r\n\tthree@y.test\r\nSubject: hi\r\n\r\nb\r\n";
    let out = transmitted_copy(raw);
    assert_eq!(s(&out), "To: a@x.test\r\nSubject: hi\r\n\r\nb\r\n");
}

#[test]
fn every_bcc_field_comes_off_whatever_its_case_or_spacing() {
    let raw = b"bcc: a@y.test\r\nTo: t@x.test\r\nBCC : b@y.test\r\nResent-Bcc: c@y.test\r\nCc: c@x.test\r\nBcc:\r\n  d@y.test\r\n\r\nbody\r\n";
    let out = s(&transmitted_copy(raw)).to_string();
    assert_eq!(out, "To: t@x.test\r\nCc: c@x.test\r\n\r\nbody\r\n");
    for addr in ["a@y", "b@y", "c@y", "d@y"] {
        assert!(!out.contains(addr), "{addr} leaked: {out}");
    }
}

#[test]
fn a_bcc_in_group_syntax_comes_off_whole() {
    let raw = b"To: t@x.test\r\nBcc: Secret list: one@y.test, \"Two, Esq\" <two@y.test>;\r\nSubject: s\r\n\r\nb\r\n";
    let out = s(&transmitted_copy(raw)).to_string();
    assert_eq!(out, "To: t@x.test\r\nSubject: s\r\n\r\nb\r\n");
}

#[test]
fn a_bcc_line_in_the_body_is_the_body() {
    let raw = b"To: t@x.test\r\n\r\nBcc: this is prose\r\n";
    assert_eq!(transmitted_copy(raw), raw.to_vec());
}

#[test]
fn bare_lf_messages_strip_the_same() {
    let raw = b"To: t@x.test\nBcc: h@y.test\n  more@y.test\nSubject: s\n\nb\n";
    assert_eq!(s(&transmitted_copy(raw)), "To: t@x.test\nSubject: s\n\nb\n");
}

#[test]
fn a_header_named_like_bcc_is_not_bcc() {
    let raw = b"X-Bcc-Note: keep\r\nBccx: keep\r\n\r\nb";
    assert_eq!(transmitted_copy(raw), raw.to_vec());
}

// ---- envelope ----------------------------------------------------------------

#[test]
fn the_envelope_is_every_to_cc_and_bcc_address_once() {
    let raw = b"From: me@x.test\r\nTo: \"Doe, Jane\" <Jane@X.test>, bob@x.test\r\nCc: carol@x.test\r\nBcc: Hidden <h1@y.test>,\r\n h2@y.test\r\nBcc: team: t1@y.test, t2@y.test;\r\nReply-To: never@z.test\r\nSubject: s\r\n\r\nTo: body@not.test\r\n";
    assert_eq!(
        envelope_recipients(raw),
        vec![
            "jane@x.test",
            "bob@x.test",
            "carol@x.test",
            "h1@y.test",
            "h2@y.test",
            "t1@y.test",
            "t2@y.test",
        ]
    );
}

#[test]
fn duplicates_and_empty_groups_add_nobody() {
    let raw = b"To: a@x.test, A@X.TEST\r\nCc: undisclosed-recipients:;\r\nBcc: a@x.test\r\n\r\n";
    assert_eq!(envelope_recipients(raw), vec!["a@x.test"]);
}

#[test]
fn a_message_with_no_recipient_has_an_empty_envelope() {
    assert!(envelope_recipients(b"Subject: s\r\n\r\nb").is_empty());
    assert!(envelope_recipients(b"Subject: s").is_empty());
}

// ---- the headers Gmail used to add -------------------------------------------

#[test]
fn missing_from_date_and_message_id_are_added_and_present_ones_kept() {
    let now = chrono::DateTime::from_timestamp(1_790_000_000, 0).unwrap();
    let out = with_submission_headers(
        b"To: a@x.test\r\n\r\nb",
        "me@x.test",
        now,
        "<m@passband.app>",
    );
    let text = s(&out);
    assert!(text.starts_with("From: me@x.test\r\nDate: "));
    assert!(text.contains("Message-ID: <m@passband.app>\r\nTo: a@x.test\r\n\r\nb"));

    let mine = b"From: Me <me@x.test>\r\nDate: Tue, 1 Sep 2026 10:00:00 +0000\r\nMessage-ID: <keep@x>\r\nTo: a@x.test\r\n\r\nb";
    assert_eq!(
        with_submission_headers(mine, "other@x.test", now, "<m@passband.app>"),
        mine.to_vec()
    );
}

#[test]
fn a_minted_message_id_is_48_hex_at_the_account_domain() {
    let id = mint_message_id("Me@Example.ORG").unwrap();
    let bare = id
        .trim_start_matches('<')
        .trim_end_matches("@example.org>");
    assert_eq!(bare.len(), 48, "{id}");
    assert!(bare.chars().all(|c| c.is_ascii_hexdigit()));
    assert_ne!(id, mint_message_id("me@example.org").unwrap());
}

#[test]
fn an_unusable_account_domain_falls_back_to_passband_app() {
    for account in [
        "no-at-sign",
        "me@",
        "me@localhost",
        "me@[127.0.0.1]",
        "me@exa mple.org",
        "me@-bad.org",
        "me@a..b",
        "me@x\"y.org",
    ] {
        let id = mint_message_id(account).unwrap();
        assert!(id.ends_with("@passband.app>"), "{account}: {id}");
    }
}

// ---- a scripted SMTP server --------------------------------------------------

/// What the scripted server does after the message's final dot.
#[derive(Clone, Copy)]
pub(crate) enum AfterDot {
    Accept,
    /// Read the message, then hang up without a reply.
    HangUp,
    Refuse554,
    Refuse451,
}

#[derive(Default)]
pub(crate) struct Seen {
    pub connections: AtomicUsize,
    pub datas: AtomicUsize,
    pub auths: AtomicUsize,
    pub rcpts: std::sync::Mutex<Vec<String>>,
    pub messages: std::sync::Mutex<Vec<String>>,
}

/// A plaintext SMTP server on loopback: just enough of RFC 5321 to take one
/// message per connection, refusing `bad@` recipients and any password but
/// `good-pw`.
pub(crate) async fn scripted(after_dot: AfterDot) -> (u16, Arc<Seen>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Seen::default());
    let seen2 = seen.clone();
    tokio::spawn(async move {
        loop {
            let Ok((tcp, _)) = listener.accept().await else {
                return;
            };
            let seen = seen2.clone();
            seen.connections.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                let (r, mut w) = tcp.into_split();
                let mut r = BufReader::new(r);
                w.write_all(b"220 scripted ESMTP\r\n").await.unwrap();
                let mut line = String::new();
                loop {
                    line.clear();
                    if r.read_line(&mut line).await.unwrap_or(0) == 0 {
                        return;
                    }
                    let cmd = line.trim_end().to_string();
                    let upper = cmd.to_ascii_uppercase();
                    let reply: &[u8] = if upper.starts_with("EHLO") {
                        b"250-scripted\r\n250-8BITMIME\r\n250 AUTH PLAIN LOGIN\r\n"
                    } else if upper.starts_with("AUTH PLAIN") {
                        seen.auths.fetch_add(1, Ordering::SeqCst);
                        use base64::Engine as _;
                        let arg = cmd.split_whitespace().nth(2).unwrap_or("");
                        let dec = base64::engine::general_purpose::STANDARD
                            .decode(arg)
                            .unwrap_or_default();
                        if dec.ends_with(b"\0good-pw") {
                            b"235 ok\r\n"
                        } else {
                            b"535 5.7.8 bad credentials for good-pw-hint\r\n"
                        }
                    } else if upper.starts_with("MAIL FROM") {
                        b"250 ok\r\n"
                    } else if upper.starts_with("RCPT TO") {
                        seen.rcpts.lock().unwrap().push(cmd.clone());
                        if upper.contains("BAD@") {
                            b"550 5.1.1 no such user\r\n"
                        } else {
                            b"250 ok\r\n"
                        }
                    } else if upper == "DATA" {
                        seen.datas.fetch_add(1, Ordering::SeqCst);
                        w.write_all(b"354 go\r\n").await.unwrap();
                        let mut msg = String::new();
                        loop {
                            line.clear();
                            if r.read_line(&mut line).await.unwrap_or(0) == 0 {
                                return;
                            }
                            if line == ".\r\n" {
                                break;
                            }
                            msg.push_str(&line);
                        }
                        seen.messages.lock().unwrap().push(msg);
                        match after_dot {
                            AfterDot::Accept => b"250 queued\r\n",
                            AfterDot::HangUp => return,
                            AfterDot::Refuse554 => b"554 5.7.1 rejected\r\n",
                            AfterDot::Refuse451 => b"451 4.3.0 try later\r\n",
                        }
                    } else if upper == "QUIT" {
                        let _ = w.write_all(b"221 bye\r\n").await;
                        return;
                    } else {
                        b"250 ok\r\n"
                    };
                    if w.write_all(reply).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    (port, seen)
}

fn endpoint(port: u16, password: &str) -> SmtpEndpoint {
    let config = ImapAccountConfig {
        server: squelch_core::mail::imap::ImapServer {
            host: "127.0.0.1".into(),
            port: 1,
            tls: ImapTls::Plain,
            username: "me@x.test".into(),
        },
        ca_pem: None,
        smtp: Some(SmtpServer {
            host: "127.0.0.1".into(),
            port,
            tls: ImapTls::Plain,
        }),
    };
    SmtpEndpoint::from_config(&config, &ImapPassword::new(password))
        .unwrap()
        .unwrap()
}

fn quick() -> SmtpTimeouts {
    SmtpTimeouts {
        connect: Duration::from_secs(5),
        command: Duration::from_secs(5),
    }
}

const MSG: &[u8] = b"To: a@x.test\r\nSubject: s\r\n\r\n.leading dot\r\nbody\r\n";

#[tokio::test]
async fn an_accepted_message_is_sent_once_and_dot_stuffed() {
    let (port, seen) = scripted(AfterDot::Accept).await;
    let rcpts = vec!["a@x.test".to_string()];
    submit(
        &endpoint(port, "good-pw"),
        quick(),
        "me@x.test",
        &rcpts,
        MSG,
        Duration::from_secs(5),
    )
    .await
    .unwrap();
    assert_eq!(seen.datas.load(Ordering::SeqCst), 1);
    let msgs = seen.messages.lock().unwrap();
    assert!(msgs[0].contains("\r\n..leading dot\r\n"), "{}", msgs[0]);
}

/// NEVER RETRY AFTER DATA (contract §7): a connection that dies after the
/// message was handed over is "may have been sent", on ONE connection with
/// ONE DATA, and nothing tries again.
#[tokio::test]
async fn a_failure_after_data_is_maybe_sent_and_never_retried() {
    let (port, seen) = scripted(AfterDot::HangUp).await;
    let rcpts = vec!["a@x.test".to_string()];
    let err = submit(
        &endpoint(port, "good-pw"),
        quick(),
        "me@x.test",
        &rcpts,
        MSG,
        Duration::from_secs(5),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, SmtpError::MaybeSent(_)), "{err:?}");
    assert!(err.to_string().contains("may have been sent"));
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(seen.connections.load(Ordering::SeqCst), 1);
    assert_eq!(seen.datas.load(Ordering::SeqCst), 1);
}

/// The server's own answer to the final dot is definite: refused, not sent.
#[tokio::test]
async fn a_refusal_of_the_message_itself_is_not_sent() {
    let (port, _) = scripted(AfterDot::Refuse554).await;
    let rcpts = vec!["a@x.test".to_string()];
    let err = submit(
        &endpoint(port, "good-pw"),
        quick(),
        "me@x.test",
        &rcpts,
        MSG,
        Duration::from_secs(5),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(&err, SmtpError::Refused(m) if m.contains("554")),
        "{err:?}"
    );
    let (port, _) = scripted(AfterDot::Refuse451).await;
    let err = submit(
        &endpoint(port, "good-pw"),
        quick(),
        "me@x.test",
        &rcpts,
        MSG,
        Duration::from_secs(5),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(&err, SmtpError::NotSent(m) if m.contains("451")),
        "{err:?}"
    );
}

/// One refused recipient stops the whole send before DATA: no subset of the
/// audience gets it.
#[tokio::test]
async fn a_refused_recipient_stops_the_send_before_data() {
    let (port, seen) = scripted(AfterDot::Accept).await;
    let rcpts = vec!["a@x.test".to_string(), "bad@x.test".to_string()];
    let err = submit(
        &endpoint(port, "good-pw"),
        quick(),
        "me@x.test",
        &rcpts,
        MSG,
        Duration::from_secs(5),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(&err, SmtpError::Refused(m) if m.contains("550")),
        "{err:?}"
    );
    assert_eq!(seen.datas.load(Ordering::SeqCst), 0);
}

/// A refused AUTH is one fixed line: no server text, no password.
#[tokio::test]
async fn a_refused_login_says_one_fixed_line() {
    let (port, seen) = scripted(AfterDot::Accept).await;
    let err = check_login(&endpoint(port, "wrong-pw-xyzzy"), quick())
        .await
        .unwrap_err();
    assert_eq!(err, SmtpError::Auth);
    let text = format!("{err} {err:?}");
    assert_eq!(err.to_string(), SMTP_LOGIN_REFUSED);
    assert!(
        !text.contains("xyzzy") && !text.contains("good-pw-hint"),
        "{text}"
    );
    assert_eq!(seen.auths.load(Ordering::SeqCst), 1);
    assert_eq!(seen.datas.load(Ordering::SeqCst), 0);
    check_login(&endpoint(port, "good-pw"), quick())
        .await
        .unwrap();
}

#[tokio::test]
async fn a_bad_address_or_no_recipient_is_refused_before_connecting() {
    let (port, seen) = scripted(AfterDot::Accept).await;
    let ep = endpoint(port, "good-pw");
    let err = submit(
        &ep,
        quick(),
        "me@x.test",
        &["a@x.test\r\nRCPT TO:<z@z>".to_string()],
        MSG,
        Duration::from_secs(5),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, SmtpError::NotSent(_)));
    let err = submit(&ep, quick(), "me@x.test", &[], MSG, Duration::from_secs(5))
        .await
        .unwrap_err();
    assert!(matches!(err, SmtpError::NotSent(_)));
    assert_eq!(seen.connections.load(Ordering::SeqCst), 0);
}

#[test]
fn plaintext_smtp_off_this_machine_is_refused() {
    let mut ep = endpoint(25, "pw");
    ep.server.host = "smtp.example.org".into();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let err = rt.block_on(check_login(&ep, quick())).unwrap_err();
    assert!(
        matches!(&err, SmtpError::NotSent(m) if m.contains("only loopback")),
        "{err:?}"
    );
}

/// A server that does not offer STARTTLS on a STARTTLS account is refused
/// before any AUTH: the password never goes in the clear.
#[tokio::test]
async fn starttls_that_the_server_does_not_offer_is_refused_before_auth() {
    let (port, seen) = scripted(AfterDot::Accept).await;
    let mut ep = endpoint(port, "good-pw");
    ep.server.tls = ImapTls::StartTls;
    let err = check_login(&ep, quick()).await.unwrap_err();
    assert!(
        matches!(&err, SmtpError::NotSent(m) if m.contains("STARTTLS")),
        "{err:?}"
    );
    assert_eq!(seen.auths.load(Ordering::SeqCst), 0);
}
