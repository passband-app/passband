use super::*;

fn mem() -> Arc<dyn MessageLocations> {
    Arc::new(MemLocations::default())
}

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
fn a_header_block_without_a_message_id_gets_the_canonical_id() {
    let with = b"Subject: hi\r\nMessage-ID:\r\n <m@Host.example>\r\n\r\n";
    let (id, mid) = stable_id_for_header_block(with);
    assert_eq!(mid.as_deref(), Some("m@host.example"));
    assert_eq!(id, stable_id_for_message_id("m@host.example"));

    let without = b"Subject: hi\r\nFrom: a@b.c\r\n\r\n";
    let (id1, mid) = stable_id_for_header_block(without);
    assert_eq!(mid, None);
    assert_eq!(id1, canonical_header_id(without));
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
fn a_stale_location_is_searched_for_in_archive_and_all_mail_too() {
    let roles = resolve_roles(&names(&[
        ("INBOX", &[]),
        ("Sent", &["\\Sent"]),
        ("Junk", &["\\Junk"]),
        ("Archive", &["\\Archive"]),
        ("[Gmail]/All Mail", &["\\All"]),
        ("Projects", &[]),
    ]));
    assert_eq!(
        roles.search_folders(),
        vec!["INBOX", "Sent", "Junk", "Archive", "[Gmail]/All Mail"]
    );
    // By name when there is no SPECIAL-USE, and never twice.
    let roles = resolve_roles(&names(&[("INBOX", &[]), ("All Mail", &[])]));
    assert_eq!(roles.search_folders(), vec!["INBOX", "All Mail"]);
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
            1,
            server(host, ImapTls::Plain),
            ImapPassword::new("pw"),
            mem(),
            refusals.clone(),
        )
        .unwrap_err();
        assert!(matches!(err, CoreError::InvalidInput(_)), "{host}");
    }
    for host in ["127.0.0.1", "localhost", "::1", "[::1]"] {
        assert!(
            ImapSource::new(
                1,
                server(host, ImapTls::Plain),
                ImapPassword::new("pw"),
                mem(),
                refusals.clone()
            )
            .is_ok(),
            "{host}"
        );
    }
    // TLS to anywhere is fine.
    assert!(
        ImapSource::new(
            1,
            server("imap.example.com", ImapTls::Implicit),
            ImapPassword::new("pw"),
            mem(),
            refusals
        )
        .is_ok()
    );
}

#[test]
fn the_password_never_reaches_debug_output() {
    let src = ImapSource::new(
        1,
        server("127.0.0.1", ImapTls::Plain),
        ImapPassword::new("hunter2-secret"),
        mem(),
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
    dev_source_for(DEV_USER, tls, port, password, mem())
}

fn dev_source_for(
    user: &str,
    tls: ImapTls,
    port: u16,
    password: &str,
    locations: Arc<dyn MessageLocations>,
) -> ImapSource {
    ImapSource::with_roots(
        1,
        ImapServer {
            host: if tls == ImapTls::Plain {
                "127.0.0.1".into()
            } else {
                "localhost".into()
            },
            port,
            tls,
            username: user.into(),
        },
        ImapPassword::new(password),
        locations,
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
        1,
        ImapServer {
            host: "localhost".into(),
            port: 10993,
            tls: ImapTls::Implicit,
            username: DEV_USER.into(),
        },
        ImapPassword::new(DEV_PASSWORD),
        mem(),
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

// ---- step 2: ids without a Message-ID (Amendment E) -------------------------

/// The same message as the author wrote it, then as delivery decorated it.
const SENT_COPY: &[u8] = b"From: Me <ME@dev.test>\r\n\
To: b@example.org, a@example.org\r\n\
Subject:  Lunch? \r\n\
Date: Mon, 26 Oct 2026 09:00:00 -0700\r\n\r\n";
const INBOX_COPY: &[u8] = b"Return-Path: <me@dev.test>\r\n\
Delivered-To: me@dev.test\r\n\
Received: from mx.dev.test by imap.dev.test; Mon, 26 Oct 2026 16:00:02 +0000\r\n\
Received: from laptop by mx.dev.test; Mon, 26 Oct 2026 16:00:01 +0000\r\n\
From: me@dev.test\r\n\
To: A <a@example.org>,\r\n\x20b@EXAMPLE.org\r\n\
Subject: Lunch?\r\n\
Date: Mon, 26 Oct 2026 16:00:00 +0000\r\n\
X-Spam-Score: 0.1\r\n\r\n";

#[test]
fn a_self_sent_copy_with_trace_headers_has_the_same_canonical_id() {
    let (sent, mid) = stable_id_for_header_block(SENT_COPY);
    assert_eq!(mid, None);
    let (inbox, _) = stable_id_for_header_block(INBOX_COPY);
    // Received/Delivered-To/Return-Path, a reordered and re-cased To, a
    // display name, the Date in another zone and the Subject's padding are
    // all delivery's or the server's, never the author's.
    assert_eq!(sent, inbox);
    assert!(sent.starts_with(STABLE_ID_PREFIX));
}

#[test]
fn the_canonical_id_still_tells_different_messages_apart() {
    let base = canonical_header_id(SENT_COPY);
    for other in [
        // Another second.
        b"From: me@dev.test\r\nTo: a@example.org, b@example.org\r\nSubject: Lunch?\r\nDate: Mon, 26 Oct 2026 09:00:01 -0700\r\n\r\n".as_slice(),
        // Another subject.
        b"From: me@dev.test\r\nTo: a@example.org, b@example.org\r\nSubject: Dinner?\r\nDate: Mon, 26 Oct 2026 09:00:00 -0700\r\n\r\n",
        // Someone moved from To to Cc.
        b"From: me@dev.test\r\nTo: a@example.org\r\nCc: b@example.org\r\nSubject: Lunch?\r\nDate: Mon, 26 Oct 2026 09:00:00 -0700\r\n\r\n",
        // Another author.
        b"From: you@dev.test\r\nTo: a@example.org, b@example.org\r\nSubject: Lunch?\r\nDate: Mon, 26 Oct 2026 09:00:00 -0700\r\n\r\n",
    ] {
        assert_ne!(base, canonical_header_id(other));
    }
    // And a Message-ID, when there is one, is what decides.
    let with_mid = b"Message-ID: <x@dev.test>\r\nSubject: Lunch?\r\n\r\n";
    assert_eq!(
        stable_id_for_header_block(with_mid).0,
        stable_id_for_message_id("x@dev.test")
    );
}

// ---- step 2: the log ceiling (Amendment F) --------------------------------

/// async-imap traces the LOGIN line, password and all, and message bodies.
/// The `max_level_debug` feature on `log` removes every trace call from the
/// build; this fails if a dependency change ever lifts the ceiling.
#[test]
fn log_trace_is_compiled_out() {
    assert!(log::STATIC_MAX_LEVEL <= log::LevelFilter::Debug);
    assert!(!log::log_enabled!(log::Level::Trace));
}

// ---- step 2: cursor merge and comparison ----------------------------------

fn pos(mailbox: &str, uidvalidity: u32, last_uid: u32, modseq: Option<u64>) -> FolderPos {
    FolderPos {
        mailbox: mailbox.into(),
        uidvalidity,
        last_uid,
        highestmodseq: modseq,
    }
}

fn cursor_of(entries: &[(&str, FolderPos)]) -> ImapCursor {
    ImapCursor {
        v: 1,
        folders: entries
            .iter()
            .map(|(k, p)| (k.to_string(), p.clone()))
            .collect(),
    }
}

#[test]
fn two_walks_merge_per_folder_and_never_step_back() {
    let since = cursor_of(&[
        ("inbox", pos("INBOX", 1, 10, Some(50))),
        ("sent", pos("Sent", 2, 4, Some(9))),
    ]);
    let mut inbox_walk = since.clone();
    inbox_walk
        .folders
        .insert("inbox".into(), pos("INBOX", 1, 13, Some(55)));
    let mut sent_walk = since.clone();
    sent_walk
        .folders
        .insert("sent".into(), pos("Sent", 2, 5, Some(10)));
    let merged = merge_cursor_values(Some(&since), &inbox_walk, &sent_walk);
    assert_eq!(merged.folders["inbox"], pos("INBOX", 1, 13, Some(55)));
    assert_eq!(merged.folders["sent"], pos("Sent", 2, 5, Some(10)));
    // Order does not matter.
    assert_eq!(
        merge_cursor_values(Some(&since), &sent_walk, &inbox_walk),
        merged
    );
    // Both sides moved one folder (never the engine's shape): the further one.
    let mut further = since.clone();
    further
        .folders
        .insert("inbox".into(), pos("INBOX", 1, 20, Some(60)));
    assert_eq!(
        merge_cursor_values(Some(&since), &inbox_walk, &further).folders["inbox"].last_uid,
        20
    );
    // A folder only one side has is kept.
    let mut with_spam = inbox_walk.clone();
    with_spam
        .folders
        .insert("spam".into(), pos("Junk", 3, 1, None));
    assert!(
        merge_cursor_values(Some(&since), &with_spam, &sent_walk)
            .folders
            .contains_key("spam")
    );
}

#[test]
fn merge_through_the_trait_survives_an_unreadable_side() {
    let src = ImapSource::new(
        1,
        server("127.0.0.1", ImapTls::Plain),
        ImapPassword::new("pw"),
        mem(),
        Arc::new(SourceRefusals::default()),
    )
    .unwrap();
    let good = cursor_of(&[("inbox", pos("INBOX", 1, 3, None))]).encode();
    let junk = SyncCursor::new("not a cursor");
    assert_eq!(src.merge_cursors(&good, &good, &junk), good);
    assert_eq!(src.merge_cursors(&good, &junk, &good), good);
    assert!(src.cursor_is_usable(&good));
    assert!(!src.cursor_is_usable(&junk));
    assert!(!src.cursor_is_usable(&cursor_of(&[("sent", pos("Sent", 1, 3, None))]).encode()));
}

#[test]
fn a_position_is_unchanged_only_when_nothing_arrived_and_modseq_agrees() {
    let p = pos("INBOX", 1, 10, Some(50));
    assert!(p.unchanged_at(&pos("INBOX", 1, 10, Some(50))));
    assert!(!p.unchanged_at(&pos("INBOX", 1, 11, Some(50))), "arrival");
    assert!(
        !p.unchanged_at(&pos("INBOX", 1, 10, Some(51))),
        "flags moved"
    );
    assert!(!p.unchanged_at(&pos("INBOX", 2, 10, Some(50))), "reset");
    assert!(!p.unchanged_at(&pos("Inbox2", 1, 10, Some(50))), "renamed");
    assert!(
        !p.unchanged_at(&pos("INBOX", 1, 10, None)),
        "lost CONDSTORE"
    );
    // Without CONDSTORE, a UIDNEXT that has not moved is all there is.
    assert!(pos("INBOX", 1, 10, None).unchanged_at(&pos("INBOX", 1, 10, None)));
}

// ---- step 2: windows, placement, errors, backoff ---------------------------

#[test]
fn the_archive_window_skips_junk_trash_and_drafts_and_filed_only_the_live_folders() {
    let roles = resolve_roles(&names(&[
        ("INBOX", &[]),
        ("Sent", &["\\Sent"]),
        ("Junk", &["\\Junk"]),
        ("Trash", &["\\Trash"]),
        ("Drafts", &["\\Drafts"]),
        ("Archive", &["\\Archive"]),
        ("Projects/2026", &[]),
        ("Projects", &["\\Noselect"]),
    ]));
    assert_eq!(
        roles.window_folders(false),
        vec!["INBOX", "Sent", "Archive", "Projects/2026"]
    );
    assert_eq!(roles.window_folders(true), vec!["Archive", "Projects/2026"]);
}

#[test]
fn placement_comes_from_every_live_row() {
    let roles = resolve_roles(&names(&[("INBOX", &[]), ("Sent", &["\\Sent"])]));
    let row = |f: &str| MessageLocation {
        folder: f.into(),
        uidvalidity: 1,
        uid: 1,
        message_id: None,
    };
    assert_eq!(
        placement_for(&roles, &[row("INBOX"), row("Sent")]),
        Placement {
            in_inbox: true,
            in_sent: true
        }
    );
    assert_eq!(
        placement_for(&roles, &[row("Archive")]),
        Placement::default()
    );
}

#[test]
fn only_transport_errors_retire_the_session() {
    use async_imap::error::Error as E;
    for keeps in [
        E::No("[NONEXISTENT] no such mailbox".into()),
        E::Bad("unknown command".into()),
        E::Validate(async_imap::error::ValidateError('\n')),
    ] {
        assert!(!is_session_broken(&imap_err("x")(keeps)));
    }
    for breaks in [
        E::Io(std::io::Error::other("reset")),
        E::ConnectionLost,
        E::Parse(async_imap::error::ParseError::Unexpected("?".into())),
    ] {
        assert!(is_session_broken(&imap_err("x")(breaks)));
    }
    assert!(!is_session_broken(&CoreError::NotFound));
    assert!(!is_session_broken(&net_err("x", "y")));
}

#[test]
fn reconnects_back_off_exponentially_to_a_cap() {
    let secs: Vec<u64> = (0..10).map(|n| backoff_delay(n).as_secs()).collect();
    assert_eq!(secs, vec![0, 1, 2, 4, 8, 16, 32, 60, 60, 60]);
    assert_eq!(backoff_delay(u32::MAX), BACKOFF_CAP);
}

/// No Dovecot needed: a port nothing listens on. The first failure is
/// immediate; the second connect waits out the first step of the backoff.
#[tokio::test]
async fn a_failed_connect_backs_off_before_the_next() {
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let src = ImapSource::new(
        1,
        ImapServer {
            host: "127.0.0.1".into(),
            port,
            tls: ImapTls::Plain,
            username: "u".into(),
        },
        ImapPassword::new("pw"),
        mem(),
        Arc::new(SourceRefusals::default()),
    )
    .unwrap();
    let t = std::time::Instant::now();
    assert!(src.inbox_unread().await.is_err());
    assert!(
        t.elapsed() < Duration::from_millis(800),
        "first try is not delayed"
    );
    let t = std::time::Instant::now();
    assert!(src.inbox_unread().await.is_err());
    assert!(
        t.elapsed() >= Duration::from_millis(900),
        "{:?}",
        t.elapsed()
    );
}

// ---- step 2: threading of IMAP mail (Amendment B) --------------------------
//
// The source returns `thread_id: None`; ingest's `fallback_thread_id` keys a
// message on its References root, else In-Reply-To, else its own Message-ID.
// These pin what that does with IMAP-shaped mail, arrival order included.

fn thread_of(raw: &str) -> String {
    let (id, _) = stable_id_for_header_block(raw.as_bytes());
    let fetched = crate::sync::ingest::RawFetched {
        account_id: 1,
        gmail_msg_id: id,
        gmail_thread_id: None,
        raw: raw.as_bytes().to_vec(),
        internal_date: Some(Utc::now()),
        is_sent: false,
        is_spam: false,
        account_addr: DEV_USER.into(),
    };
    crate::sync::ingest::ingest(
        &fetched,
        &crate::config::Stage1Config::default(),
        Utc::now(),
        |_| false,
    )
    .message
    .thread_id
}

const PARENT: &str = "From: a@x.org\r\nTo: me@dev.test\r\nSubject: Plan\r\nMessage-ID: <root@x.org>\r\n\r\nparent\r\n";

#[test]
fn a_reply_that_arrives_before_its_parent_still_joins_its_thread() {
    let reply = "From: b@x.org\r\nTo: me@dev.test\r\nSubject: Re: Plan\r\nMessage-ID: <r1@x.org>\r\nIn-Reply-To: <root@x.org>\r\nReferences: <root@x.org>\r\n\r\nreply\r\n";
    // Each is keyed from its own headers, so the order they are ingested in
    // cannot matter: the reply's References root IS the parent's Message-ID.
    let first = thread_of(reply);
    let second = thread_of(PARENT);
    assert_eq!(first, second);
    assert_eq!(first, "root@x.org");
}

#[test]
fn a_reply_without_references_threads_by_in_reply_to() {
    let reply = "From: b@x.org\r\nSubject: Re: Plan\r\nMessage-ID: <r1@x.org>\r\nIn-Reply-To: <root@x.org>\r\n\r\nreply\r\n";
    assert_eq!(thread_of(reply), thread_of(PARENT));
    // One level deeper with In-Reply-To only, the key is the IMMEDIATE
    // parent, not the root: this splits, by design (no store lookup).
    let deeper = "From: a@x.org\r\nSubject: Re: Plan\r\nMessage-ID: <r2@x.org>\r\nIn-Reply-To: <r1@x.org>\r\n\r\nreply 2\r\n";
    assert_eq!(thread_of(deeper), "r1@x.org");
}

#[test]
fn a_truncated_references_header_keys_on_what_is_left() {
    // A client that trimmed the root off a long chain: the first id left is
    // the key, so this message starts a thread apart from the root's.
    let trimmed = "From: c@x.org\r\nSubject: Re: Plan\r\nMessage-ID: <r9@x.org>\r\nIn-Reply-To: <r8@x.org>\r\nReferences: <r7@x.org> <r8@x.org>\r\n\r\nlate\r\n";
    assert_eq!(thread_of(trimmed), "r7@x.org");
    assert_ne!(thread_of(trimmed), thread_of(PARENT));
    // RFC 5322 trimming keeps the first id, which keeps the thread.
    let kept_root = "From: c@x.org\r\nSubject: Re: Plan\r\nMessage-ID: <r9@x.org>\r\nIn-Reply-To: <r8@x.org>\r\nReferences: <root@x.org> <r8@x.org>\r\n\r\nlate\r\n";
    assert_eq!(thread_of(kept_root), thread_of(PARENT));
}

#[test]
fn mail_with_no_ids_at_all_is_its_own_thread_under_its_stable_id() {
    let lone = "From: n@x.org\r\nTo: me@dev.test\r\nSubject: hi\r\nDate: Mon, 26 Oct 2026 09:00:00 +0000\r\n\r\nbody\r\n";
    let (id, _) = stable_id_for_header_block(lone.as_bytes());
    assert_eq!(thread_of(lone), id);
}

// ---- step 2 live: each test gets a fresh, empty mailbox -------------------
//
// The test bed's passdb is static (one password for any user), so a new
// username is a new, empty mailbox with the SPECIAL-USE folders created.

fn fresh_user(tag: &str) -> String {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("t-{tag}-{n}@dev.test")
}

/// A second, read-write connection: the "other client" that delivers,
/// moves and rebuilds folders under the source's feet.
async fn other_client(user: &str) -> ImapSession {
    let tcp = TcpStream::connect(("127.0.0.1", 10143)).await.unwrap();
    let mut client = Client::new(Transport::Plain(tcp));
    read_greeting(&mut client).await.unwrap();
    client
        .login(user, DEV_PASSWORD)
        .await
        .map_err(|e| e.0)
        .unwrap()
}

fn mail(mid: Option<&str>, subject: &str) -> Vec<u8> {
    let mut m = String::from("From: a@x.org\r\nTo: me@dev.test\r\n");
    m.push_str(&format!(
        "Subject: {subject}\r\nDate: Mon, 26 Oct 2026 09:00:00 +0000\r\n"
    ));
    if let Some(mid) = mid {
        m.push_str(&format!("Message-ID: <{mid}>\r\n"));
    }
    m.push_str("\r\nbody\r\n");
    m.into_bytes()
}

async fn append(s: &mut ImapSession, folder: &str, raw: &[u8], date: Option<&str>) {
    s.append(folder, None, date, raw).await.unwrap();
}

fn live(user: &str) -> (ImapSource, Arc<MemLocations>) {
    let locs = Arc::new(MemLocations::default());
    (
        dev_source_for(user, ImapTls::Plain, 10143, DEV_PASSWORD, locs.clone()),
        locs,
    )
}

fn inbox_listing() -> Listing {
    Listing::Folder {
        folder: Folder::Inbox,
        within_days: None,
    }
}

#[tokio::test]
#[ignore = "needs the Dovecot test bed: scripts/imap-dev.sh start"]
async fn live_mail_that_lands_between_the_listing_and_the_cursor_is_the_next_walks() {
    let user = fresh_user("gap");
    let mut other = other_client(&user).await;
    append(&mut other, INBOX, &mail(Some("a@x.org"), "a"), None).await;
    append(&mut other, INBOX, &mail(Some("b@x.org"), "b"), None).await;
    let (src, _) = live(&user);

    let listed = src
        .list(inbox_listing(), None, Urgency::Window)
        .await
        .unwrap();
    assert_eq!(listed.len(), 2);
    // Delivered mid-backfill: after the listing, before the cursor.
    append(&mut other, INBOX, &mail(Some("late@x.org"), "late"), None).await;
    let cursor = src.current_cursor().await.unwrap();

    let walk = src.changes(&cursor, Folder::Inbox).await.unwrap();
    assert_eq!(walk.ids, vec![stable_id_for_message_id("late@x.org")]);
    assert!(!walk.mailbox_unchanged);
    // And the walk's cursor has it, so the one after finds nothing.
    let again = src.changes(&walk.cursor, Folder::Inbox).await.unwrap();
    assert!(again.ids.is_empty());
    assert!(again.mailbox_unchanged);

    // The snapshot was spent: a second cursor is live STATUS.
    let live_cursor = ImapCursor::decode(&src.current_cursor().await.unwrap()).unwrap();
    assert_eq!(live_cursor.folders["inbox"].last_uid, 3);
    let _ = other.logout().await;
}

#[tokio::test]
#[ignore = "needs the Dovecot test bed: scripts/imap-dev.sh start"]
async fn live_the_change_feed_walks_each_folder_and_proves_an_idle_mailbox() {
    let user = fresh_user("feed");
    let mut other = other_client(&user).await;
    let (src, _) = live(&user);
    let start = src.current_cursor().await.unwrap();
    assert!(src.cursor_is_usable(&start));

    let idle = src.changes(&start, Folder::Inbox).await.unwrap();
    assert!(idle.ids.is_empty() && idle.mailbox_unchanged);

    // Mail the user sent from their phone: INBOX has nothing, but the
    // mailbox moved, so the engine must go on to the SENT walk.
    append(
        &mut other,
        "Sent",
        &mail(Some("phone@dev.test"), "from phone"),
        None,
    )
    .await;
    let inbox = src.changes(&start, Folder::Inbox).await.unwrap();
    assert!(inbox.ids.is_empty());
    assert!(!inbox.mailbox_unchanged);
    let sent = src.changes(&start, Folder::Sent).await.unwrap();
    assert_eq!(sent.ids, vec![stable_id_for_message_id("phone@dev.test")]);

    let merged = src.merge_cursors(&start, &inbox.cursor, &sent.cursor);
    let after = src.changes(&merged, Folder::Inbox).await.unwrap();
    assert!(after.ids.is_empty() && after.mailbox_unchanged);

    // A flag change elsewhere (read on the phone) is seen through
    // HIGHESTMODSEQ as "changed", never as new mail.
    other.select("Sent").await.unwrap();
    other
        .uid_store("1:*", "+FLAGS (\\Seen)")
        .await
        .unwrap()
        .try_collect::<Vec<_>>()
        .await
        .unwrap();
    let flagged = src.changes(&merged, Folder::Inbox).await.unwrap();
    assert!(flagged.ids.is_empty());
    assert!(!flagged.mailbox_unchanged);
    let _ = other.logout().await;
}

#[tokio::test]
#[ignore = "needs the Dovecot test bed: scripts/imap-dev.sh start"]
async fn live_a_rebuilt_folder_expires_the_cursor() {
    let user = fresh_user("reset");
    let mut other = other_client(&user).await;
    append(&mut other, "Sent", &mail(Some("s1@dev.test"), "s1"), None).await;
    let (src, _) = live(&user);
    let start = src.current_cursor().await.unwrap();

    // Recreating a folder is how a UIDVALIDITY reset happens in practice.
    other.delete("Sent").await.unwrap();
    other.create("Sent").await.unwrap();
    append(&mut other, "Sent", &mail(Some("s1@dev.test"), "s1"), None).await;
    assert!(matches!(
        src.changes(&start, Folder::Sent).await,
        Err(ChangesError::Expired)
    ));
    // INBOX's own position is still good, and says the mailbox moved.
    let inbox = src.changes(&start, Folder::Inbox).await.unwrap();
    assert!(!inbox.mailbox_unchanged);

    // A role that resolves to another folder is a reset too (Amendment D).
    let mut moved = ImapCursor::decode(&start).unwrap();
    moved.folders.get_mut("sent").unwrap().mailbox = "Old Sent".into();
    assert!(matches!(
        src.changes(&moved.encode(), Folder::Sent).await,
        Err(ChangesError::Expired)
    ));
    let _ = other.logout().await;
}

#[tokio::test]
#[ignore = "needs the Dovecot test bed: scripts/imap-dev.sh start"]
async fn live_a_moved_message_keeps_its_id_and_its_rows_follow_it() {
    let user = fresh_user("move");
    let mut other = other_client(&user).await;
    append(&mut other, INBOX, &mail(Some("mv@x.org"), "to junk"), None).await;
    append(
        &mut other,
        INBOX,
        &mail(Some("arc@x.org"), "to archive"),
        None,
    )
    .await;
    let (src, locs) = live(&user);
    let ids = src
        .list(inbox_listing(), None, Urgency::Live)
        .await
        .unwrap();
    let (to_junk, to_archive) = (
        stable_id_for_message_id("mv@x.org"),
        stable_id_for_message_id("arc@x.org"),
    );
    assert_eq!(ids.len(), 2);

    other.select(INBOX).await.unwrap();
    other.uid_mv("1", "Junk").await.unwrap();
    other.uid_mv("2", "Archive").await.unwrap();

    // Into a watched folder: the Message-ID search finds it, under the same id.
    let m = src.fetch_raw(&to_junk, Urgency::Live).await.unwrap();
    assert!(matches!(m.raw, RawBody::Bytes(_)));
    assert_eq!(m.placement, Placement::default());
    let rows = locs.message_locations(1, &to_junk).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].folder, "Junk");

    // Archived from another client: the search widens to the archive and
    // all-mail folders, so the body is still there under the same id and the
    // row follows it. Reading this as gone would leave the blank-body heal
    // and the recipients backfill with nothing for good.
    let m = src.fetch_raw(&to_archive, Urgency::Live).await.unwrap();
    assert!(matches!(m.raw, RawBody::Bytes(_)));
    assert_eq!(m.placement, Placement::default());
    let rows = locs.message_locations(1, &to_archive).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].folder, "Archive");
    let h = src.fetch_headers(&to_archive, &["Subject"]).await.unwrap();
    assert_eq!(h.headers, vec![("Subject".into(), "to archive".into())]);
    // The archive window sees it under the same id: no duplicate row.
    let window = src
        .list(
            Listing::Window {
                from: Utc::now() - chrono::Duration::days(1),
                to: Utc::now() + chrono::Duration::days(1),
                filed_only: true,
            },
            None,
            Urgency::Background,
        )
        .await
        .unwrap();
    assert_eq!(window, vec![to_archive.clone()]);

    // Filed somewhere the search does not look (a user folder): Missing, not
    // an error that would fail a whole ingest batch, and the stale row goes.
    other.create("Projects").await.unwrap();
    other.select("Archive").await.unwrap();
    other.uid_mv("1:*", "Projects").await.unwrap();
    let m = src.fetch_raw(&to_archive, Urgency::Live).await.unwrap();
    assert!(matches!(m.raw, RawBody::Missing));
    assert!(locs.message_locations(1, &to_archive).unwrap().is_empty());
    // Never seen at all is NotFound.
    assert!(matches!(
        src.fetch_raw("imap:nope", Urgency::Live).await,
        Err(CoreError::NotFound)
    ));
    let _ = other.logout().await;
}

#[tokio::test]
#[ignore = "needs the Dovecot test bed: scripts/imap-dev.sh start"]
async fn live_a_self_sent_message_is_one_id_in_inbox_and_sent() {
    let user = fresh_user("self");
    let mut other = other_client(&user).await;
    // One with a Message-ID, one without (Amendment E): delivery adds trace
    // headers to the INBOX copy only.
    let with = mail(Some("self@dev.test"), "note to self");
    append(&mut other, "Sent", &with, None).await;
    append(&mut other, INBOX, &with, None).await;
    append(&mut other, "Sent", SENT_COPY, None).await;
    append(&mut other, INBOX, INBOX_COPY, None).await;
    let (src, locs) = live(&user);
    let mut inbox = src
        .list(inbox_listing(), None, Urgency::Live)
        .await
        .unwrap();
    let mut sent = src
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
    inbox.sort();
    sent.sort();
    assert_eq!(inbox, sent);
    assert_eq!(inbox.len(), 2);
    for id in &inbox {
        assert_eq!(locs.message_locations(1, id).unwrap().len(), 2);
        let m = src.fetch_raw(id, Urgency::Live).await.unwrap();
        assert!(m.placement.in_inbox && m.placement.in_sent, "{id}");
    }
    let _ = other.logout().await;
}

#[tokio::test]
#[ignore = "needs the Dovecot test bed: scripts/imap-dev.sh start"]
async fn live_the_archive_window_is_exact_at_its_edges() {
    let user = fresh_user("window");
    let mut other = other_client(&user).await;
    for (folder, mid, date) in [
        ("Archive", "in@x.org", "10-Mar-2025 12:00:00 +0000"),
        ("Archive", "edge-from@x.org", "10-Mar-2025 00:00:00 +0000"),
        // `to` is exclusive.
        ("Archive", "edge-to@x.org", "11-Mar-2025 00:00:00 +0000"),
        // Same UTC day, another zone: still inside.
        ("Archive", "zone@x.org", "10-Mar-2025 22:00:00 -0100"),
        ("Archive", "before@x.org", "09-Mar-2025 23:59:59 +0000"),
        (INBOX, "inbox@x.org", "10-Mar-2025 13:00:00 +0000"),
        ("Sent", "sent@x.org", "10-Mar-2025 14:00:00 +0000"),
        ("Trash", "trash@x.org", "10-Mar-2025 12:00:00 +0000"),
        ("Junk", "junk@x.org", "10-Mar-2025 12:00:00 +0000"),
        ("Drafts", "draft@x.org", "10-Mar-2025 12:00:00 +0000"),
    ] {
        append(
            &mut other,
            folder,
            &mail(Some(mid), mid),
            Some(&format!("\"{date}\"")),
        )
        .await;
    }
    let (src, _) = live(&user);
    let from = chrono::DateTime::parse_from_rfc3339("2025-03-10T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let to = from + chrono::Duration::days(1);
    let ids =
        |v: &[&str]| -> Vec<String> { v.iter().map(|m| stable_id_for_message_id(m)).collect() };
    let filed = src
        .list(
            Listing::Window {
                from,
                to,
                filed_only: true,
            },
            None,
            Urgency::Background,
        )
        .await
        .unwrap();
    // Newest first: zone (23:00Z), in (12:00Z), edge-from (00:00Z).
    assert_eq!(filed, ids(&["zone@x.org", "in@x.org", "edge-from@x.org"]));
    let mut all = src
        .list(
            Listing::Window {
                from,
                to,
                filed_only: false,
            },
            None,
            Urgency::Background,
        )
        .await
        .unwrap();
    all.sort();
    let mut want = ids(&[
        "zone@x.org",
        "in@x.org",
        "edge-from@x.org",
        "inbox@x.org",
        "sent@x.org",
    ]);
    want.sort();
    assert_eq!(all, want);
    let capped = src
        .list(
            Listing::Window {
                from,
                to,
                filed_only: false,
            },
            Some(2),
            Urgency::Background,
        )
        .await
        .unwrap();
    assert_eq!(capped, ids(&["zone@x.org", "sent@x.org"]));
    let _ = other.logout().await;
}

#[tokio::test]
#[ignore = "needs the Dovecot test bed: scripts/imap-dev.sh start"]
async fn live_a_refused_command_keeps_the_session_and_a_dropped_call_does_not_poison_it() {
    let user = fresh_user("conn");
    let mut other = other_client(&user).await;
    append(&mut other, INBOX, &mail(Some("c@x.org"), "c"), None).await;
    let (src, locs) = live(&user);
    src.inbox_unread().await.unwrap();
    assert_eq!(src.connects(), 1);

    // A NO from the server, a missing message, a stale row in a folder that
    // is gone: none of them is the connection's fault.
    assert!(
        src.run_command_for_test("EXAMINE \"No Such Folder\"")
            .await
            .is_err()
    );
    assert!(matches!(
        src.fetch_raw("imap:unknown", Urgency::Live).await,
        Err(CoreError::NotFound)
    ));
    locs.record_message_locations(
        1,
        &[(
            "imap:ghost".into(),
            MessageLocation {
                folder: "Gone".into(),
                uidvalidity: 1,
                uid: 1,
                message_id: Some("ghost@x.org".into()),
            },
        )],
    )
    .unwrap();
    assert!(matches!(
        src.fetch_raw("imap:ghost", Urgency::Live)
            .await
            .unwrap()
            .raw,
        RawBody::Missing
    ));
    assert!(matches!(
        src.fetch_headers("imap:ghost", &["Subject"]).await,
        Err(CoreError::NotFound)
    ));
    src.inbox_unread().await.unwrap();
    assert_eq!(src.connects(), 1, "the session survived all of that");

    // A caller dropped mid-command (the engine's fetch window does this):
    // the next call must not read the half-finished response.
    src.list(inbox_listing(), None, Urgency::Live)
        .await
        .unwrap();
    let id = stable_id_for_message_id("c@x.org");
    for _ in 0..20 {
        let _ = tokio::time::timeout(
            Duration::from_micros(200),
            src.fetch_raw(&id, Urgency::Live),
        )
        .await;
        let m = src.fetch_raw(&id, Urgency::Live).await.unwrap();
        assert!(matches!(m.raw, RawBody::Bytes(_)));
    }
    // (Measured 2026-10-28: 10 of the 20 drops landed mid-command and were
    // retired; the count is timing-dependent, so it is not asserted.)
    let _ = other.logout().await;
}

#[tokio::test]
#[ignore = "needs the Dovecot test bed: scripts/imap-dev.sh start"]
async fn live_the_sqlite_store_is_a_location_table() {
    use crate::store::sqlite::SqliteStore;
    let user = fresh_user("sqlite");
    let mut other = other_client(&user).await;
    append(&mut other, INBOX, &mail(Some("db@x.org"), "db"), None).await;
    let store = Arc::new(SqliteStore::open_in_memory().unwrap());
    let acct = store.ensure_account(&user).unwrap();
    let src = ImapSource::with_roots(
        acct,
        ImapServer {
            host: "127.0.0.1".into(),
            port: 10143,
            tls: ImapTls::Plain,
            username: user.clone(),
        },
        ImapPassword::new(DEV_PASSWORD),
        store.clone(),
        Arc::new(SourceRefusals::default()),
        &[],
    )
    .unwrap();
    let ids = src
        .list(inbox_listing(), None, Urgency::Live)
        .await
        .unwrap();
    assert_eq!(ids, vec![stable_id_for_message_id("db@x.org")]);
    let rows = store.message_locations(acct, &ids[0]).unwrap();
    assert_eq!(rows[0].message_id.as_deref(), Some("db@x.org"));
    // A fresh source over the same store (a daemon restart) fetches it
    // without listing first.
    drop(src);
    let src = ImapSource::with_roots(
        acct,
        ImapServer {
            host: "127.0.0.1".into(),
            port: 10143,
            tls: ImapTls::Plain,
            username: user,
        },
        ImapPassword::new(DEV_PASSWORD),
        store,
        Arc::new(SourceRefusals::default()),
        &[],
    )
    .unwrap();
    assert!(matches!(
        src.fetch_raw(&ids[0], Urgency::Live).await.unwrap().raw,
        RawBody::Bytes(_)
    ));
    let _ = other.logout().await;
}

// ---- step 3 live: IDLE ------------------------------------------------------

/// Hang up every connection `user` has open, the way a server restart or a
/// NAT timeout would.
fn kick(user: &str) {
    let out = std::process::Command::new("docker")
        .args(["exec", "squelch-imap-dev", "doveadm", "kick", user])
        .output()
        .expect("docker exec");
    assert!(
        out.status.success(),
        "doveadm kick: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[tokio::test]
#[ignore = "needs the Dovecot test bed: scripts/imap-dev.sh start"]
async fn live_idle_wakes_on_an_append_within_seconds() {
    let user = fresh_user("idle");
    let mut other = other_client(&user).await;
    let (src, _) = live(&user);
    let src = Arc::new(src);
    let cursor = src.current_cursor().await.unwrap();

    let waiter = {
        let (src, cursor) = (src.clone(), cursor.clone());
        tokio::spawn(async move {
            let t = tokio::time::Instant::now();
            src.wait_for_change(Some(&cursor), Duration::from_secs(60))
                .await;
            t.elapsed()
        })
    };
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert!(!waiter.is_finished(), "parked, not polling");
    append(&mut other, INBOX, &mail(Some("idle@x.org"), "idle"), None).await;
    let took = tokio::time::timeout(Duration::from_secs(5), waiter)
        .await
        .expect("IDLE woke within 5s of the APPEND")
        .unwrap();
    assert!(took < Duration::from_secs(5), "{took:?}");
    assert_eq!(src.idle_connects(), 1);

    // The command session saw none of that and still works.
    let walk = src.changes(&cursor, Folder::Inbox).await.unwrap();
    assert_eq!(walk.ids, vec![stable_id_for_message_id("idle@x.org")]);

    // The engine hands back the walked cursor: the next wait parks on the
    // SAME idle connection (no new login).
    let t = tokio::time::Instant::now();
    src.wait_for_change(Some(&walk.cursor), Duration::from_secs(2))
        .await;
    assert!(t.elapsed() >= Duration::from_millis(1900));
    assert_eq!(src.idle_connects(), 1);
    let _ = other.logout().await;
}

#[tokio::test]
#[ignore = "needs the Dovecot test bed: scripts/imap-dev.sh start"]
async fn live_idle_returns_at_once_when_the_server_is_ahead_but_never_twice() {
    let user = fresh_user("ahead");
    let mut other = other_client(&user).await;
    let (src, _) = live(&user);
    let stale = src.current_cursor().await.unwrap();
    append(&mut other, INBOX, &mail(Some("early@x.org"), "early"), None).await;

    let t = tokio::time::Instant::now();
    src.wait_for_change(Some(&stale), Duration::from_secs(30))
        .await;
    assert!(
        t.elapsed() < Duration::from_secs(3),
        "mail landed before the park"
    );
    // The same cursor again: the engine's walk did not move it, so this
    // parks rather than spin.
    let t = tokio::time::Instant::now();
    src.wait_for_change(Some(&stale), Duration::from_secs(2))
        .await;
    assert!(t.elapsed() >= Duration::from_millis(1900));
    let _ = other.logout().await;
}

#[tokio::test]
#[ignore = "needs the Dovecot test bed: scripts/imap-dev.sh start"]
async fn live_a_wait_dropped_mid_idle_leaves_nothing_broken() {
    let user = fresh_user("drop");
    let mut other = other_client(&user).await;
    let (src, _) = live(&user);
    let cursor = src.current_cursor().await.unwrap();

    // Shutdown or a manual refresh wins the select! mid-IDLE.
    for _ in 0..3 {
        let r = tokio::time::timeout(
            Duration::from_millis(500),
            src.wait_for_change(Some(&cursor), Duration::from_secs(60)),
        )
        .await;
        assert!(r.is_err(), "still parked when dropped");
    }
    // Each dropped wait took its connection with it; the next logs in anew
    // and still wakes on mail.
    assert_eq!(src.idle_connects(), 3);
    let src = Arc::new(src);
    let waiter = {
        let (src, cursor) = (src.clone(), cursor.clone());
        tokio::spawn(async move {
            src.wait_for_change(Some(&cursor), Duration::from_secs(60))
                .await
        })
    };
    tokio::time::sleep(Duration::from_millis(800)).await;
    append(&mut other, INBOX, &mail(Some("after@x.org"), "after"), None).await;
    tokio::time::timeout(Duration::from_secs(5), waiter)
        .await
        .expect("woke after the drops")
        .unwrap();
    // The command session is unaffected.
    assert!(src.inbox_unread().await.is_ok());
    let _ = other.logout().await;
}

#[tokio::test]
#[ignore = "needs the Dovecot test bed: scripts/imap-dev.sh start"]
async fn live_a_killed_idle_connection_sleeps_out_the_wait_and_never_spins() {
    let user = fresh_user("kick");
    let (src, _) = live(&user);
    let cursor = src.current_cursor().await.unwrap();

    let src = Arc::new(src);
    let waiter = {
        let (src, cursor) = (src.clone(), cursor.clone());
        tokio::spawn(async move {
            let t = tokio::time::Instant::now();
            src.wait_for_change(Some(&cursor), Duration::from_secs(4))
                .await;
            t.elapsed()
        })
    };
    tokio::time::sleep(Duration::from_secs(1)).await;
    kick(&user);
    let took = waiter.await.unwrap();
    // Not back at the moment of the hang-up: the rest of max_wait is slept.
    assert!(took >= Duration::from_millis(3900), "{took:?}");
    assert!(took < Duration::from_secs(6), "{took:?}");

    // A server refusing the login is the same: one attempt, then sleep.
    let bad = dev_source_for(&user, ImapTls::Plain, 10143, "wrong", mem());
    let t = tokio::time::Instant::now();
    bad.wait_for_change(None, Duration::from_secs(2)).await;
    assert!(t.elapsed() >= Duration::from_millis(1900));

    // And the next wait reconnects and parks normally.
    let t = tokio::time::Instant::now();
    src.wait_for_change(Some(&cursor), Duration::from_secs(2))
        .await;
    assert!(t.elapsed() >= Duration::from_millis(1900));
    assert_eq!(src.idle_connects(), 2);
}

fn doveadm(args: &[&str]) {
    let out = std::process::Command::new("docker")
        .args(["exec", "squelch-imap-dev", "doveadm"])
        .args(args)
        .output()
        .expect("docker exec");
    assert!(
        out.status.success(),
        "doveadm {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Found by the step 4 end-to-end run: deleting INBOX under a parked IDLE
/// left Dovecot holding the connection open and silent, so the wait never
/// came back (the daemon's poll lane sat in it until a manual refresh). The
/// wait is bounded by `max_wait` however the server behaves.
#[tokio::test]
#[ignore = "needs the Dovecot test bed: scripts/imap-dev.sh start"]
async fn live_a_wait_returns_by_its_deadline_when_inbox_is_deleted_under_idle() {
    const WAIT: Duration = Duration::from_secs(15);
    let user = fresh_user("delinbox");
    let mut other = other_client(&user).await;
    append(
        &mut other,
        INBOX,
        &mail(Some("doomed@x.org"), "doomed"),
        None,
    )
    .await;
    let (src, _) = live(&user);
    let cursor = src.current_cursor().await.unwrap();
    let src = Arc::new(src);
    let waiter = {
        let (src, cursor) = (src.clone(), cursor.clone());
        tokio::spawn(async move {
            let t = tokio::time::Instant::now();
            src.wait_for_change(Some(&cursor), WAIT).await;
            t.elapsed()
        })
    };
    // Past `imap_hibernate_timeout` (5s on the bed): the parked IDLE has been
    // handed to imap-hibernate, and the delete wakes it into a process that
    // cannot reopen INBOX. That is the silent connection.
    tokio::time::sleep(Duration::from_secs(8)).await;
    doveadm(&["mailbox", "delete", "-u", &user, INBOX]);
    let took = tokio::time::timeout(WAIT + IDLE_GRACE * 2, waiter)
        .await
        .expect("the wait came back")
        .unwrap();
    assert!(
        took < WAIT + IDLE_GRACE + Duration::from_secs(1),
        "{took:?}"
    );
    // The next wait still works once INBOX is back.
    doveadm(&["mailbox", "create", "-u", &user, INBOX]);
    let t = tokio::time::Instant::now();
    src.wait_for_change(Some(&cursor), Duration::from_secs(30))
        .await;
    assert!(
        t.elapsed() < Duration::from_secs(5) + IDLE_GRACE,
        "{:?}",
        t.elapsed()
    );
    let _ = other.logout().await;
}

/// No Dovecot needed: a server that answers the login and the EXAMINE, says
/// `+ idling`, and then never says anything again (not even to `DONE`), the
/// way the bed's imap-hibernate does once INBOX is deleted. The wait still
/// comes back by its deadline plus [`IDLE_GRACE`].
#[tokio::test]
async fn a_server_that_goes_silent_under_idle_cannot_hold_the_wait() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        let (sock, _) = listener.accept().await.unwrap();
        let (r, mut w) = sock.into_split();
        let mut lines = BufReader::new(r).lines();
        w.write_all(b"* OK silent test server\r\n").await.unwrap();
        while let Ok(Some(line)) = lines.next_line().await {
            let (tag, cmd) = line.split_once(' ').unwrap_or((line.as_str(), ""));
            let cmd = cmd.to_ascii_uppercase();
            let reply = if cmd.starts_with("CAPABILITY") {
                format!("* CAPABILITY IMAP4rev1 IDLE\r\n{tag} OK done\r\n")
            } else if cmd.starts_with("EXAMINE") {
                format!(
                    "* 0 EXISTS\r\n* OK [UIDVALIDITY 7] ok\r\n* OK [UIDNEXT 1] ok\r\n\
                     {tag} OK [READ-ONLY] done\r\n"
                )
            } else if cmd.starts_with("IDLE") {
                // Then silence, DONE included, with the socket held open.
                w.write_all(b"+ idling\r\n").await.unwrap();
                std::future::pending::<()>().await;
                unreachable!()
            } else {
                format!("{tag} OK done\r\n")
            };
            w.write_all(reply.as_bytes()).await.unwrap();
        }
    });
    let src = ImapSource::new(
        1,
        ImapServer {
            host: "127.0.0.1".into(),
            port,
            tls: ImapTls::Plain,
            username: "u".into(),
        },
        ImapPassword::new("pw"),
        mem(),
        Arc::new(SourceRefusals::default()),
    )
    .unwrap();
    let t = tokio::time::Instant::now();
    tokio::time::timeout(
        Duration::from_secs(1) + IDLE_GRACE * 2,
        src.wait_for_change(None, Duration::from_secs(1)),
    )
    .await
    .expect("the wait is bounded");
    let took = t.elapsed();
    assert!(took >= Duration::from_millis(900), "parked first: {took:?}");
    assert!(took < Duration::from_secs(2) + IDLE_GRACE, "{took:?}");
    server.abort();
}
