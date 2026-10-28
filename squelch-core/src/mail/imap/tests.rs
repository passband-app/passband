use super::*;

#[test]
fn a_message_id_is_normalized_on_its_domain_only() {
    assert_eq!(
        normalize_message_id("  <Abc.DEF@Example.ORG> ").as_deref(),
        Some("Abc.DEF@example.org")
    );
    assert_eq!(
        normalize_message_id("abc@EXAMPLE.org").as_deref(),
        Some("abc@example.org")
    );
    // A trailing comment after the bracketed id is not part of it.
    assert_eq!(
        normalize_message_id("<a@b.c> (sent by x)").as_deref(),
        Some("a@b.c")
    );
    assert_eq!(normalize_message_id("  <>  "), None);
    assert_eq!(normalize_message_id(""), None);
}

#[test]
fn the_stable_id_ignores_spelling_differences_a_server_may_add() {
    let a = stable_id_for_message_id(&normalize_message_id("<x@Example.org>").unwrap());
    let b = stable_id_for_message_id(&normalize_message_id("x@example.ORG").unwrap());
    assert_eq!(a, b);
    assert!(a.starts_with(STABLE_ID_PREFIX));
    assert_eq!(a.len(), STABLE_ID_PREFIX.len() + 64);
    // The local part IS case-sensitive.
    let c = stable_id_for_message_id(&normalize_message_id("<X@example.org>").unwrap());
    assert_ne!(a, c);
}

#[test]
fn a_header_block_without_a_message_id_hashes_the_block() {
    let with = b"Subject: hi\r\nMessage-ID:\r\n <m@Host.example>\r\n\r\n";
    let (id, mid) = stable_id_for_header_block(with);
    assert_eq!(mid.as_deref(), Some("m@host.example"));
    assert_eq!(id, stable_id_for_message_id("m@host.example"));

    let without = b"Subject: hi\r\nFrom: a@b.c\r\n\r\n";
    let (id1, mid) = stable_id_for_header_block(without);
    assert_eq!(mid, None);
    let (id2, _) = stable_id_for_header_block(b"Subject: hi!\r\nFrom: a@b.c\r\n\r\n");
    assert_ne!(id1, id2, "different blocks must not share an id");
    assert_eq!(
        id1,
        stable_id_for_header_block(without).0,
        "and it is stable"
    );
}

#[test]
fn a_header_block_unfolds_in_order() {
    let h = b"To: a@b.c,\r\n\t d@e.f\r\nCc: x@y.z\r\nTo: second@b.c\r\n\r\n";
    assert_eq!(
        parse_header_block(h),
        vec![
            ("To".to_string(), "a@b.c, d@e.f".to_string()),
            ("Cc".to_string(), "x@y.z".to_string()),
            ("To".to_string(), "second@b.c".to_string()),
        ]
    );
}

fn names(list: &[(&str, &[&str])]) -> Vec<(String, Vec<String>)> {
    list.iter()
        .map(|(n, a)| (n.to_string(), a.iter().map(|s| s.to_string()).collect()))
        .collect()
}

#[test]
fn special_use_wins_over_a_folder_named_like_the_role() {
    let roles = resolve_roles(&names(&[
        ("INBOX", &[]),
        ("Sent", &[]),
        ("Gesendet", &["\\Sent"]),
        ("Spam", &[]),
        ("[Gmail]", &["\\Noselect"]),
    ]));
    assert_eq!(roles.sent.as_deref(), Some("Gesendet"));
    // No \Junk attribute anywhere: the name fallback finds Spam.
    assert_eq!(roles.junk.as_deref(), Some("Spam"));
    assert_eq!(roles.archive, None);
    assert!(!roles.selectable.iter().any(|n| n == "[Gmail]"));
    assert_eq!(
        roles.watched(),
        vec![
            (Folder::Inbox, "INBOX"),
            (Folder::Sent, "Gesendet"),
            (Folder::Spam, "Spam")
        ]
    );
}

#[test]
fn a_server_with_no_junk_folder_watches_two() {
    let roles = resolve_roles(&names(&[("INBOX", &[]), ("Sent Messages", &[])]));
    assert_eq!(roles.sent.as_deref(), Some("Sent Messages"));
    assert_eq!(roles.watched().len(), 2);
}

#[test]
fn the_cursor_round_trips_and_refuses_what_it_did_not_write() {
    let mut c = ImapCursor {
        v: 1,
        ..Default::default()
    };
    c.folders.insert(
        "inbox".into(),
        FolderPos {
            mailbox: "INBOX".into(),
            uidvalidity: 7,
            last_uid: 42,
            highestmodseq: Some(99),
        },
    );
    let enc = c.encode();
    assert_eq!(ImapCursor::decode(&enc), Some(c));
    for bad in ["", "12345", "{}", r#"{"v":2,"folders":{}}"#] {
        assert_eq!(ImapCursor::decode(&SyncCursor::new(bad)), None, "{bad:?}");
    }
}

fn server(host: &str, tls: ImapTls) -> ImapServer {
    ImapServer {
        host: host.into(),
        port: 143,
        tls,
        username: "u".into(),
    }
}

#[test]
fn plaintext_is_refused_off_loopback() {
    let refusals = Arc::new(SourceRefusals::default());
    for host in ["imap.example.com", "10.0.0.1", "localhost.example.com"] {
        let err = ImapSource::new(
            server(host, ImapTls::Plain),
            ImapPassword::new("pw"),
            refusals.clone(),
        )
        .unwrap_err();
        assert!(matches!(err, CoreError::InvalidInput(_)), "{host}");
    }
    for host in ["127.0.0.1", "localhost", "::1", "[::1]"] {
        assert!(
            ImapSource::new(
                server(host, ImapTls::Plain),
                ImapPassword::new("pw"),
                refusals.clone()
            )
            .is_ok(),
            "{host}"
        );
    }
    // TLS to anywhere is fine.
    assert!(
        ImapSource::new(
            server("imap.example.com", ImapTls::Implicit),
            ImapPassword::new("pw"),
            refusals
        )
        .is_ok()
    );
}

#[test]
fn the_password_never_reaches_debug_output() {
    let src = ImapSource::new(
        server("127.0.0.1", ImapTls::Plain),
        ImapPassword::new("hunter2-secret"),
        Arc::new(SourceRefusals::default()),
    )
    .unwrap();
    let dbg = format!("{src:?} {:?}", ImapPassword::new("hunter2-secret"));
    assert!(!dbg.contains("hunter2"), "{dbg}");
}

// ---- live: the Dovecot test bed (scripts/imap-dev.sh start && seed) -------
//
//   cargo test -p squelch-core mail::imap -- --ignored --test-threads=1

const DEV_USER: &str = "squelch@dev.test";
const DEV_PASSWORD: &str = "squelch-dev-password";

fn dev_ca() -> CertificateDer<'static> {
    use tokio_rustls::rustls::pki_types::pem::PemObject;
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../scripts/imap-dev/.state/ca.crt"
    );
    let pem = std::fs::read(path).expect("scripts/imap-dev.sh start mints the CA");
    CertificateDer::from_pem_slice(&pem).expect("CA pem")
}

fn dev_source(tls: ImapTls, port: u16, password: &str) -> ImapSource {
    ImapSource::with_roots(
        ImapServer {
            host: if tls == ImapTls::Plain {
                "127.0.0.1".into()
            } else {
                "localhost".into()
            },
            port,
            tls,
            username: DEV_USER.into(),
        },
        ImapPassword::new(password),
        Arc::new(SourceRefusals::default()),
        &[dev_ca()],
    )
    .unwrap()
}

/// `\Seen` on every INBOX message, read on a separate connection so the
/// source under test does not observe itself.
async fn inbox_seen_flags() -> Vec<bool> {
    let tcp = TcpStream::connect(("127.0.0.1", 10143)).await.unwrap();
    let mut client = Client::new(Transport::Plain(tcp));
    read_greeting(&mut client).await.unwrap();
    let mut s = client
        .login(DEV_USER, DEV_PASSWORD)
        .await
        .map_err(|e| e.0)
        .unwrap();
    s.examine(INBOX).await.unwrap();
    let fetches: Vec<Fetch> = s
        .uid_fetch("1:*", "(UID FLAGS)")
        .await
        .unwrap()
        .try_collect()
        .await
        .unwrap();
    let out = fetches
        .iter()
        .map(|f| {
            f.flags()
                .any(|fl| matches!(fl, async_imap::types::Flag::Seen))
        })
        .collect();
    s.logout().await.unwrap();
    out
}

#[tokio::test]
#[ignore = "needs the Dovecot test bed: scripts/imap-dev.sh start && scripts/imap-dev.sh seed"]
async fn live_lists_and_fetches_the_seeded_inbox_without_marking_it_read() {
    let src = dev_source(ImapTls::Plain, 10143, DEV_PASSWORD);
    let roles = src.roles().await.unwrap();
    assert_eq!(roles.sent.as_deref(), Some("Sent"));
    assert_eq!(roles.junk.as_deref(), Some("Junk"));
    assert_eq!(roles.archive.as_deref(), Some("Archive"));

    let ids = src
        .list(
            Listing::Folder {
                folder: Folder::Inbox,
                within_days: None,
            },
            None,
            Urgency::Live,
        )
        .await
        .unwrap();
    assert_eq!(ids.len(), 5, "the seed puts five messages in INBOX");
    assert!(ids.iter().all(|i| i.starts_with(STABLE_ID_PREFIX)));
    let parent = stable_id_for_message_id("thread-parent@example.net");
    assert!(ids.contains(&parent));
    // The domain was `Example.ORG` on the wire.
    assert!(ids.contains(&stable_id_for_message_id("plain-1@example.org")));

    let mut subjects = Vec::new();
    for id in &ids {
        let m = src.fetch_raw(id, Urgency::Live).await.unwrap();
        assert_eq!(&m.id, id);
        assert!(m.thread_id.is_none());
        assert!(m.placement.in_inbox && !m.placement.in_sent);
        assert!(m.received_at.is_some());
        let RawBody::Bytes(raw) = m.raw else {
            panic!("{id}: no body")
        };
        let parsed = mail_parser::MessageParser::default()
            .parse(&raw)
            .expect("parses");
        subjects.push(parsed.subject().unwrap_or_default().to_string());
    }
    subjects.sort();
    assert_eq!(
        subjects,
        vec![
            "Multipart fixture",
            "No Message-ID here",
            "Plain fixture",
            "Re: Thread parent",
            "Thread parent"
        ]
    );

    let h = src
        .fetch_headers(&parent, &["To", "Message-ID"])
        .await
        .unwrap();
    assert!(h.headers.iter().any(|(n, v)| n == "To" && v == DEV_USER));
    assert_eq!(h.headers.len(), 2);

    assert_eq!(src.inbox_unread().await.unwrap(), (5, 5));
    assert!(
        inbox_seen_flags().await.iter().all(|seen| !seen),
        "BODY.PEEK must leave \\Seen unset"
    );

    let sent = src
        .list(
            Listing::Folder {
                folder: Folder::Sent,
                within_days: None,
            },
            None,
            Urgency::Live,
        )
        .await
        .unwrap();
    assert_eq!(sent, vec![stable_id_for_message_id("sent-1@dev.test")]);
    let m = src.fetch_raw(&sent[0], Urgency::Live).await.unwrap();
    assert!(m.placement.in_sent && !m.placement.in_inbox);

    let cursor = src.current_cursor().await.unwrap();
    assert!(src.cursor_is_usable(&cursor));
    let c = ImapCursor::decode(&cursor).unwrap();
    assert_eq!(c.folders["inbox"].last_uid, 5);
    assert_eq!(c.folders["sent"].last_uid, 1);
    assert_eq!(c.folders["spam"].mailbox, "Junk");
    assert!(c.folders["inbox"].highestmodseq.is_some());
}

#[tokio::test]
#[ignore = "needs the Dovecot test bed: scripts/imap-dev.sh start"]
async fn live_tls_and_starttls_verify_the_certificate_and_log_in() {
    for (tls, port) in [(ImapTls::Implicit, 10993), (ImapTls::StartTls, 10143)] {
        let src = dev_source(tls, port, DEV_PASSWORD);
        src.inbox_unread()
            .await
            .unwrap_or_else(|e| panic!("{tls:?}: {e}"));
    }
    // Without the test bed's CA the same server is refused: verification is on.
    let untrusted = ImapSource::new(
        ImapServer {
            host: "localhost".into(),
            port: 10993,
            tls: ImapTls::Implicit,
            username: DEV_USER.into(),
        },
        ImapPassword::new(DEV_PASSWORD),
        Arc::new(SourceRefusals::default()),
    )
    .unwrap();
    let err = untrusted.inbox_unread().await.unwrap_err().to_string();
    assert!(err.contains("tls handshake"), "{err}");
}

#[tokio::test]
#[ignore = "needs the Dovecot test bed: scripts/imap-dev.sh start"]
async fn live_a_wrong_password_is_a_credential_error_without_the_password() {
    let src = dev_source(ImapTls::Plain, 10143, "wrong-password-xyzzy");
    let err = src.inbox_unread().await.unwrap_err();
    assert!(matches!(err, CoreError::Credential(_)), "{err:?}");
    assert!(!err.to_string().contains("xyzzy"));
    assert_eq!(src.refusals.auth.load(Ordering::Relaxed), 1);
}
