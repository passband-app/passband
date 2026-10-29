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
    let mut client = Client::new(Transport::new(Stream::Plain(tcp), None));
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
    // The const assert in imap.rs is the real pin (it holds in release, where
    // tests do not run); this is its runtime twin.
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
    // A walk that DROPPED its folder (the role went away) wins over the
    // other walk carrying the old entry along unmoved.
    let mut dropped = since.clone();
    dropped.folders.remove("sent");
    let carried = since.clone();
    assert!(
        !merge_cursor_values(Some(&since), &carried, &dropped)
            .folders
            .contains_key("sent")
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

/// CORRECTNESS M4: an all-mail view holds INBOX and Sent mail too, so it is
/// never walked: `filed_only` would leak live mail, and every window would
/// list each message twice. By SPECIAL-USE or by its name.
#[test]
fn the_archive_window_never_walks_the_all_mail_view() {
    for all in [("All Mail", &["\\All"][..]), ("All Mail", &[][..])] {
        let roles = resolve_roles(&names(&[
            ("INBOX", &[]),
            ("Sent", &["\\Sent"]),
            all,
            ("Archive", &["\\Archive"]),
        ]));
        assert_eq!(roles.all.as_deref(), Some("All Mail"));
        assert_eq!(
            roles.window_folders(false),
            vec!["INBOX", "Sent", "Archive"]
        );
        assert_eq!(roles.window_folders(true), vec!["Archive"]);
    }
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
    let mut client = Client::new(Transport::new(Stream::Plain(tcp), None));
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
async fn live_a_rebuilt_sent_folder_is_relisted_not_a_catch_up() {
    let user = fresh_user("reset");
    let mut other = other_client(&user).await;
    append(&mut other, "Sent", &mail(Some("s1@dev.test"), "s1"), None).await;
    let (src, _) = live(&user);
    let start = src.current_cursor().await.unwrap();

    // Recreating a folder is how a UIDVALIDITY reset happens in practice.
    other.delete("Sent").await.unwrap();
    other.create("Sent").await.unwrap();
    append(&mut other, "Sent", &mail(Some("s1@dev.test"), "s1"), None).await;
    // MATRIX 19: not an Expired (whose catch-up re-fetches all of INBOX):
    // the Sent walk is Sent's own window under its new position.
    let old_v = ImapCursor::decode(&start).unwrap().folders["sent"].uidvalidity;
    let sent = src
        .changes(&start, Folder::Sent)
        .await
        .expect("a Sent reset is handled in the walk");
    assert_eq!(sent.ids, vec![stable_id_for_message_id("s1@dev.test")]);
    assert!(!sent.mailbox_unchanged);
    let after = ImapCursor::decode(&sent.cursor).unwrap();
    assert_ne!(after.folders["sent"].uidvalidity, old_v);
    // And from there it is an ordinary walk.
    let again = src.changes(&sent.cursor, Folder::Sent).await.unwrap();
    assert!(again.ids.is_empty());
    // INBOX's own position is still good, and says the mailbox moved.
    let inbox = src.changes(&start, Folder::Inbox).await.unwrap();
    assert!(!inbox.mailbox_unchanged);

    // A role that resolves to another folder is a reset too (Amendment D),
    // handled the same way.
    let mut moved = ImapCursor::decode(&start).unwrap();
    moved.folders.get_mut("sent").unwrap().mailbox = "Old Sent".into();
    let w = src
        .changes(&moved.encode(), Folder::Sent)
        .await
        .expect("relisted");
    assert_eq!(
        ImapCursor::decode(&w.cursor).unwrap().folders["sent"].mailbox,
        "Sent"
    );
    // INBOX alone still expires: its reset IS the catch-up.
    let mut inbox_reset = ImapCursor::decode(&start).unwrap();
    inbox_reset.folders.get_mut("inbox").unwrap().uidvalidity = 1;
    assert!(matches!(
        src.changes(&inbox_reset.encode(), Folder::Inbox).await,
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

    // Filed in a user folder: the last-resort search over every selectable
    // folder finds it (correctness M3), so it is not lost to the live path.
    other.create("Projects").await.unwrap();
    other.select("Archive").await.unwrap();
    other.uid_mv("1:*", "Projects").await.unwrap();
    let m = src.fetch_raw(&to_archive, Urgency::Live).await.unwrap();
    assert!(matches!(m.raw, RawBody::Bytes(_)));
    let rows = locs.message_locations(1, &to_archive).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].folder, "Projects");
    // Gone from every folder: Missing, not an error that would fail a whole
    // ingest batch, and the stale row goes.
    other.select("Projects").await.unwrap();
    other
        .uid_store("1:*", "+FLAGS (\\Deleted)")
        .await
        .unwrap()
        .try_collect::<Vec<_>>()
        .await
        .unwrap();
    other
        .expunge()
        .await
        .unwrap()
        .try_collect::<Vec<_>>()
        .await
        .unwrap();
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
    // CORRECTNESS L4: the walk takes the command session once per folder
    // (plus once for the folder list), never once for the whole walk, so
    // the poll lane can run between folders.
    let walked = src.roles().await.unwrap().window_folders(false).len() as u64;
    assert!(walked >= 3, "INBOX, Sent, Archive at least");
    let calls_before = src.command_calls().await;
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
    assert_eq!(src.command_calls().await - calls_before, walked + 1);
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
    // Not back at the moment of the hang-up: after a backoff it logs in
    // again and parks for the rest of max_wait (no mail came).
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

/// CORRECTNESS H1 against the real server: the bed's keepalive is 10s, so a
/// 35s wait sees three `* OK Still here` lines. It comes back at 35s (not at
/// the outer bound, which would also drop the connection), and a later wait
/// on the same parked connection still wakes on an APPEND.
#[tokio::test]
#[ignore = "needs the Dovecot test bed: scripts/imap-dev.sh start"]
async fn live_a_long_wait_under_keepalives_ends_on_time_and_still_wakes() {
    let user = fresh_user("keepalive");
    let mut other = other_client(&user).await;
    let (src, _) = live(&user);
    let cursor = src.current_cursor().await.unwrap();
    let t = tokio::time::Instant::now();
    src.wait_for_change(Some(&cursor), Duration::from_secs(35))
        .await;
    let took = t.elapsed();
    assert!(took >= Duration::from_millis(34_900), "{took:?}");
    assert!(took < Duration::from_secs(37), "{took:?}");
    assert_eq!(src.idle_connects(), 1);

    let src = Arc::new(src);
    let waiter = {
        let (src, cursor) = (src.clone(), cursor.clone());
        tokio::spawn(async move {
            src.wait_for_change(Some(&cursor), Duration::from_secs(60))
                .await
        })
    };
    // Two keepalives in, then mail.
    tokio::time::sleep(Duration::from_secs(22)).await;
    assert!(!waiter.is_finished(), "keepalives are not a wake");
    append(&mut other, INBOX, &mail(Some("ka@x.org"), "ka"), None).await;
    tokio::time::timeout(Duration::from_secs(5), waiter)
        .await
        .expect("woke on the APPEND")
        .unwrap();
    assert_eq!(src.idle_connects(), 1, "the same parked connection");
    let _ = other.logout().await;
}

/// CORRECTNESS L3 / step 4 scenario 11: after `doveadm kick` the wait does
/// not sleep out `max_wait`. It logs in again after a backoff and mail that
/// lands a few seconds later wakes it.
#[tokio::test]
#[ignore = "needs the Dovecot test bed: scripts/imap-dev.sh start"]
async fn live_mail_after_a_kick_wakes_the_same_wait() {
    let user = fresh_user("kickmail");
    let other = other_client(&user).await;
    let (src, _) = live(&user);
    let cursor = src.current_cursor().await.unwrap();
    let src = Arc::new(src);
    let waiter = {
        let (src, cursor) = (src.clone(), cursor.clone());
        tokio::spawn(async move {
            let t = tokio::time::Instant::now();
            src.wait_for_change(Some(&cursor), Duration::from_secs(120))
                .await;
            t.elapsed()
        })
    };
    tokio::time::sleep(Duration::from_secs(1)).await;
    kick(&user);
    // The kick took the other client too.
    tokio::time::sleep(Duration::from_secs(3)).await;
    let mut other2 = other_client(&user).await;
    append(&mut other2, INBOX, &mail(Some("kicked@x.org"), "k"), None).await;
    let took = tokio::time::timeout(Duration::from_secs(10), waiter)
        .await
        .expect("woke within seconds of the APPEND")
        .unwrap();
    assert!(took < Duration::from_secs(10), "{took:?}");
    assert_eq!(src.idle_connects(), 2, "one fresh login after the kick");
    drop(other);
    let _ = other2.logout().await;
}

/// CORRECTNESS M1: the poll lane never walks Junk, so junk mail must not
/// make an otherwise idle mailbox read as changed (that ran the Sent walk
/// and rewrote the cursor on every tick, and the idle ladder never grew).
#[tokio::test]
#[ignore = "needs the Dovecot test bed: scripts/imap-dev.sh start"]
async fn live_junk_mail_does_not_make_an_idle_mailbox_read_as_changed() {
    let user = fresh_user("junkidle");
    let mut other = other_client(&user).await;
    let (src, _) = live(&user);
    let cursor = src.current_cursor().await.unwrap();
    append(&mut other, "Junk", &mail(Some("spam@x.org"), "spam"), None).await;
    for folder in [Folder::Inbox, Folder::Sent] {
        let walk = src.changes(&cursor, folder).await.unwrap();
        assert!(walk.ids.is_empty());
        assert!(walk.mailbox_unchanged, "{folder:?}");
    }
    // Sent mail still counts.
    append(&mut other, "Sent", &mail(Some("s@x.org"), "s"), None).await;
    let walk = src.changes(&cursor, Folder::Inbox).await.unwrap();
    assert!(!walk.mailbox_unchanged);
    let _ = other.logout().await;
}

/// CORRECTNESS M5: the unread count is not read from the session's stale
/// view of an INBOX it has open.
#[tokio::test]
#[ignore = "needs the Dovecot test bed: scripts/imap-dev.sh start"]
async fn live_the_unread_count_is_fresh_when_inbox_is_open() {
    let user = fresh_user("unread");
    let mut other = other_client(&user).await;
    append(&mut other, INBOX, &mail(Some("u1@x.org"), "u1"), None).await;
    let (src, _) = live(&user);
    assert_eq!(src.inbox_unread().await.unwrap(), (1, 1));
    for n in 2..=3 {
        // The listing leaves INBOX open on the command session.
        src.list(inbox_listing(), None, Urgency::Window)
            .await
            .unwrap();
        append(
            &mut other,
            INBOX,
            &mail(Some(&format!("u{n}@x.org")), "u"),
            None,
        )
        .await;
        assert_eq!(src.inbox_unread().await.unwrap(), (n, n));
    }
    let _ = other.logout().await;
}

/// What the engine does on `ChangesError::Expired`: list INBOX and Sent,
/// then take the cursor.
async fn catch_up(src: &ImapSource) -> SyncCursor {
    for folder in [Folder::Inbox, Folder::Sent] {
        src.list(
            Listing::Folder {
                folder,
                within_days: None,
            },
            None,
            Urgency::Window,
        )
        .await
        .unwrap();
    }
    src.current_cursor().await.expect("the catch-up's cursor")
}

/// One poll tick from `cursor`, the way the engine runs it: INBOX then Sent.
/// `None` is a tick that went to a catch-up.
/// A walk that broke its session (Dovecot hangs up a session whose open
/// folder is deleted or renamed) is tried once more, on a new login, the way
/// the engine's next tick would.
async fn tick(src: &ImapSource, cursor: &SyncCursor) -> Option<(Vec<String>, SyncCursor)> {
    async fn walk(src: &ImapSource, cursor: &SyncCursor, f: Folder) -> Option<Changes> {
        for attempt in 0..2 {
            match src.changes(cursor, f).await {
                Ok(w) => return Some(w),
                Err(ChangesError::Expired) => return None,
                Err(ChangesError::Other(e)) if attempt == 0 && is_session_broken(&e) => {
                    eprintln!("{f:?} walk broke its session ({e}); next tick");
                }
                Err(ChangesError::Other(e)) => panic!("{f:?} walk: {e}"),
            }
        }
        unreachable!()
    }
    let inbox = walk(src, cursor, Folder::Inbox).await?;
    let sent = walk(src, cursor, Folder::Sent).await?;
    let mut ids = inbox.ids.clone();
    ids.extend(sent.ids.clone());
    Some((ids, src.merge_cursors(cursor, &inbox.cursor, &sent.cursor)))
}

/// Poll until the ticks stop asking for a catch-up; panics if they never do.
async fn converge(src: &ImapSource, mut cursor: SyncCursor) -> SyncCursor {
    for _ in 0..3 {
        match tick(src, &cursor).await {
            Some((_, next)) => return next,
            None => cursor = catch_up(src).await,
        }
    }
    panic!("still expiring after three catch-ups: the same failure forever");
}

/// CORRECTNESS H2: a watched folder deleted or renamed in another client.
/// The session's roles used to be resolved once per login, so the catch-up
/// the expiry sent the engine to failed ("a watched folder vanished
/// mid-sync") on the same session, the same way, forever. Now the roles are
/// listed again, the gone folder drops out of (or is re-resolved in) the
/// cursor, polling resumes, and INBOX mail still arrives.
///
/// The bed's folders are `auto = subscribe`, so Dovecot recreates Sent and
/// Junk (with a new UIDVALIDITY) the moment they are next touched: this is
/// the "re-resolved" leg. A folder that stays gone, as on a server without
/// auto-created folders, is [`a_watched_folder_that_stays_gone_drops_out_of_the_cursor`].
#[tokio::test]
#[ignore = "needs the Dovecot test bed: scripts/imap-dev.sh start"]
async fn live_a_deleted_or_renamed_watched_folder_does_not_wedge_sync() {
    let user = fresh_user("vanish");
    let mut other = other_client(&user).await;
    append(&mut other, "Sent", &mail(Some("s1@x.org"), "s1"), None).await;
    append(&mut other, "Junk", &mail(Some("j1@x.org"), "j1"), None).await;
    let (src, _) = live(&user);
    let cursor = catch_up(&src).await;
    let _ = other.logout().await;

    // Sent deleted.
    doveadm(&["mailbox", "delete", "-u", &user, "Sent"]);
    let cursor = converge(&src, cursor).await;
    let mut other = other_client(&user).await;
    append(
        &mut other,
        INBOX,
        &mail(Some("after-sent@x.org"), "a"),
        None,
    )
    .await;
    let (ids, _) = tick(&src, &cursor).await.expect("no further catch-up");
    assert_eq!(ids, vec![stable_id_for_message_id("after-sent@x.org")]);

    // Junk renamed: the catch-up's cursor must not fail on the old name.
    doveadm(&["mailbox", "rename", "-u", &user, "Junk", "OldJunk"]);
    let cursor = src
        .current_cursor()
        .await
        .expect("no vanished-folder error");
    let decoded = ImapCursor::decode(&cursor).unwrap();
    assert!(
        decoded
            .folders
            .get("spam")
            .is_none_or(|p| p.mailbox != "Junk" || p.uidvalidity != 0),
        "{decoded:?}"
    );
    let cursor = converge(&src, cursor).await;
    append(
        &mut other,
        INBOX,
        &mail(Some("after-junk@x.org"), "b"),
        None,
    )
    .await;
    let (ids, cursor) = tick(&src, &cursor).await.expect("no further catch-up");
    assert_eq!(ids, vec![stable_id_for_message_id("after-junk@x.org")]);
    // Settled: the next tick finds nothing and expires nothing.
    let (ids, _) = tick(&src, &cursor).await.expect("still no catch-up");
    assert!(ids.is_empty());
    let _ = other.logout().await;
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

// ---- scripted servers: timeouts, LOGIN failures, size caps ----------------

/// What a scripted server does with one command line.
enum Say {
    Reply(String),
    /// Hold the socket open and never answer again.
    Silence,
    /// Write this, then close the connection (a kick, a `* BYE`).
    HangUp(String),
}

/// A loopback IMAP server that greets (unless `greet` is false) and answers
/// each command with `script(tag, COMMAND LINE UPPERCASED)`. Accepts any
/// number of connections, one at a time. Returns the port and how many
/// connections it took.
async fn scripted_server(
    greet: bool,
    script: impl Fn(&str, &str) -> Say + Send + Sync + 'static,
) -> (u16, Arc<AtomicU64>, tokio::task::JoinHandle<()>) {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let accepted = Arc::new(AtomicU64::new(0));
    let count = accepted.clone();
    let script = Arc::new(script);
    let task = tokio::spawn(async move {
        loop {
            let Ok((sock, _)) = listener.accept().await else {
                return;
            };
            count.fetch_add(1, Ordering::Relaxed);
            let script = script.clone();
            tokio::spawn(async move {
                let (r, mut w) = sock.into_split();
                let mut lines = BufReader::new(r).lines();
                if !greet {
                    std::future::pending::<()>().await;
                }
                w.write_all(b"* OK scripted\r\n").await.unwrap();
                while let Ok(Some(line)) = lines.next_line().await {
                    let (tag, cmd) = line.split_once(' ').unwrap_or((line.as_str(), ""));
                    match script(tag, &cmd.to_ascii_uppercase()) {
                        Say::Reply(r) => {
                            if w.write_all(r.as_bytes()).await.is_err() {
                                return;
                            }
                        }
                        Say::Silence => std::future::pending::<()>().await,
                        Say::HangUp(r) => {
                            let _ = w.write_all(r.as_bytes()).await;
                            let _ = w.shutdown().await;
                            return;
                        }
                    }
                }
            });
        }
    });
    (port, accepted, task)
}

/// The usual answers to the login conversation, `None` for anything else.
fn login_ok(tag: &str, cmd: &str) -> Option<String> {
    if cmd.starts_with("LOGIN") {
        Some(format!("{tag} OK logged in\r\n"))
    } else if cmd.starts_with("CAPABILITY") {
        Some(format!("* CAPABILITY IMAP4rev1\r\n{tag} OK done\r\n"))
    } else if cmd.starts_with("LIST") {
        Some(format!(
            "* LIST () \"/\" INBOX\r\n* LIST (\\Sent) \"/\" Sent\r\n{tag} OK done\r\n"
        ))
    } else {
        None
    }
}

fn scripted_source(port: u16, password: &str) -> ImapSource {
    ImapSource::new(
        1,
        ImapServer {
            host: "127.0.0.1".into(),
            port,
            tls: ImapTls::Plain,
            username: "u".into(),
        },
        ImapPassword::new(password),
        mem(),
        Arc::new(SourceRefusals::default()),
    )
    .unwrap()
}

fn short_timeouts() -> Timeouts {
    Timeouts {
        connect: Duration::from_millis(500),
        login: Duration::from_millis(500),
        stall: Duration::from_millis(300),
        idle_reissue: IDLE_REISSUE,
        roles_relist: ROLES_RELIST,
    }
}

/// A SERVER THAT STOPS ANSWERING MID-COMMAND cannot hold the command session
/// (security review 3): the command fails within the stall bound, the
/// session is retired, and the next call logs in afresh.
#[tokio::test]
async fn a_server_that_goes_silent_mid_command_is_given_up_on() {
    let (port, accepted, server) = scripted_server(true, |tag, cmd| match login_ok(tag, cmd) {
        Some(r) => Say::Reply(r),
        None => Say::Silence,
    })
    .await;
    let src = scripted_source(port, "pw").with_timeouts(short_timeouts());
    for round in 1..=2u64 {
        let t = tokio::time::Instant::now();
        let err = tokio::time::timeout(Duration::from_secs(5), src.inbox_unread())
            .await
            .expect("bounded")
            .unwrap_err();
        assert!(is_session_broken(&err), "{err}");
        assert!(t.elapsed() < Duration::from_secs(2), "{:?}", t.elapsed());
        assert_eq!(src.connects(), round, "a new login each time");
    }
    assert_eq!(accepted.load(Ordering::Relaxed), 2);
    server.abort();
}

/// A server that never greets, or never finishes the login, is bounded too.
#[tokio::test]
async fn a_server_that_never_greets_is_bounded_by_the_login_timeout() {
    let (port, _, server) = scripted_server(false, |_, _| Say::Silence).await;
    let src = scripted_source(port, "pw").with_timeouts(short_timeouts());
    let t = tokio::time::Instant::now();
    let err = src.check_login().await.unwrap_err();
    assert!(t.elapsed() < Duration::from_secs(2), "{:?}", t.elapsed());
    assert!(is_session_broken(&err), "{err}");
    server.abort();

    // Greets, then sits on LOGIN.
    let (port, _, server) = scripted_server(true, |_, _| Say::Silence).await;
    let src = scripted_source(port, "pw").with_timeouts(short_timeouts());
    let t = tokio::time::Instant::now();
    assert!(src.check_login().await.is_err());
    assert!(t.elapsed() < Duration::from_secs(2), "{:?}", t.elapsed());
    server.abort();
}

/// EVERY LOGIN FAILURE IS THE SAME FIXED LINE (security review 6): a `BAD`
/// that echoes the command (the password with it) never reaches the error,
/// and it counts as an auth refusal like a `NO`.
#[tokio::test]
async fn a_login_failure_of_any_kind_says_nothing_the_server_said() {
    for reply in [
        "BAD [PARSE] could not parse: LOGIN u hunter2-secret",
        "NO [AUTHENTICATIONFAILED] hunter2-secret is wrong",
    ] {
        let reply = reply.to_string();
        let (port, _, server) = scripted_server(true, move |tag, cmd| {
            if cmd.starts_with("LOGIN") {
                Say::Reply(format!("{tag} {reply}\r\n"))
            } else {
                Say::Reply(format!("{tag} OK\r\n"))
            }
        })
        .await;
        let refusals = Arc::new(SourceRefusals::default());
        let src = ImapSource::new(
            1,
            ImapServer {
                host: "127.0.0.1".into(),
                port,
                tls: ImapTls::Plain,
                username: "u".into(),
            },
            ImapPassword::new("hunter2-secret"),
            mem(),
            refusals.clone(),
        )
        .unwrap();
        let err = src.check_login().await.unwrap_err();
        assert!(matches!(err, CoreError::Credential(_)), "{err:?}");
        assert!(err.to_string().ends_with(LOGIN_REFUSED), "{err}");
        assert!(!format!("{err:?}").contains("hunter2"));
        assert_eq!(refusals.auth.load(Ordering::Relaxed), 1);
        server.abort();
    }
}

/// A server for the fetch caps: INBOX holds UID 1, whose FETCH answers are
/// `fetch(tag, cmd)`. Records every command it was sent.
async fn fetch_server(
    fetch: impl Fn(&str, &str) -> String + Send + Sync + 'static,
) -> (
    u16,
    Arc<std::sync::Mutex<Vec<String>>>,
    tokio::task::JoinHandle<()>,
) {
    let log = Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = log.clone();
    let (port, _, task) = scripted_server(true, move |tag, cmd| {
        seen.lock().unwrap().push(cmd.to_string());
        if let Some(r) = login_ok(tag, cmd) {
            return Say::Reply(r);
        }
        Say::Reply(if cmd.starts_with("EXAMINE") {
            format!(
                "* 1 EXISTS\r\n* OK [UIDVALIDITY 7] ok\r\n* OK [UIDNEXT 2] ok\r\n\
                 {tag} OK [READ-ONLY] done\r\n"
            )
        } else if cmd.starts_with("UID FETCH") {
            fetch(tag, cmd)
        } else {
            format!("{tag} OK done\r\n")
        })
    })
    .await;
    (port, log, task)
}

fn one_row(locs: &Arc<MemLocations>, id: &str) {
    locs.record_message_locations(
        1,
        &[(
            id.to_string(),
            MessageLocation {
                folder: INBOX.into(),
                uidvalidity: 7,
                uid: 1,
                message_id: None,
            },
        )],
    )
    .unwrap();
}

/// THE SIZE IS ASKED BEFORE THE BODY (security review 4): a message over the
/// bound is refused without the body ever being requested. And refused as a
/// message the engine steps over (`Undecodable`, Gmail's answer for the same
/// bound), never an error (matrix row 20): an error fails the whole tick, the
/// next tick lists the same UID, and nothing behind it ever arrives.
#[tokio::test]
async fn an_oversized_message_is_refused_before_its_body_is_fetched() {
    let (port, log, server) = fetch_server(|tag, cmd| {
        if cmd.contains("RFC822.SIZE") {
            format!("* 1 FETCH (UID 1 RFC822.SIZE 300000000)\r\n{tag} OK done\r\n")
        } else {
            format!("* 1 FETCH (UID 1 BODY[] {{4}}\r\nboom)\r\n{tag} OK done\r\n")
        }
    })
    .await;
    let locs = Arc::new(MemLocations::default());
    one_row(&locs, "imap:big");
    let src = dev_source_for_port(port, locs);
    let m = src
        .fetch_raw("imap:big", Urgency::Live)
        .await
        .expect("an oversized message is skipped, not a failed fetch");
    match &m.raw {
        RawBody::Undecodable(why) => {
            assert!(why.contains("64MB") && why.contains("300000000"), "{why}")
        }
        _ => panic!("not refused as undecodable"),
    }
    let cmds = log.lock().unwrap().clone();
    assert!(
        !cmds.iter().any(|c| c.contains("BODY.PEEK[]")),
        "the body was asked for: {cmds:?}"
    );
    server.abort();
}

/// A body fetch is a partial one capped one byte past the bound, so a
/// server keeping to the RFC cannot send more than that.
#[tokio::test]
async fn the_body_fetch_asks_for_no_more_than_the_bound() {
    let (port, log, server) = fetch_server(|tag, cmd| {
        if cmd.contains("RFC822.SIZE") {
            format!("* 1 FETCH (UID 1 RFC822.SIZE 4)\r\n{tag} OK done\r\n")
        } else {
            format!("* 1 FETCH (UID 1 BODY[]<0> {{4}}\r\nabcd)\r\n{tag} OK done\r\n")
        }
    })
    .await;
    let locs = Arc::new(MemLocations::default());
    one_row(&locs, "imap:small");
    let src = dev_source_for_port(port, locs);
    let m = src.fetch_raw("imap:small", Urgency::Live).await.unwrap();
    assert!(matches!(&m.raw, RawBody::Bytes(b) if b == b"abcd"));
    let cmds = log.lock().unwrap().clone();
    assert!(
        cmds.iter()
            .any(|c| c.contains(&format!("BODY.PEEK[]<0.{}>", MAX_RAW_BYTES + 1))),
        "{cmds:?}"
    );
    server.abort();
}

/// A server that floods a FETCH with responses is cut off with the session,
/// never collected without bound.
#[tokio::test]
async fn a_fetch_flooded_with_responses_retires_the_session() {
    let (port, _, server) = fetch_server(|tag, _| {
        let mut r = String::new();
        for _ in 0..200 {
            r.push_str("* 1 FETCH (UID 1 RFC822.SIZE 4)\r\n");
        }
        r.push_str(&format!("{tag} OK done\r\n"));
        r
    })
    .await;
    let locs = Arc::new(MemLocations::default());
    one_row(&locs, "imap:flood");
    let src = dev_source_for_port(port, locs);
    let err = src
        .fetch_raw("imap:flood", Urgency::Live)
        .await
        .unwrap_err();
    assert!(is_session_broken(&err), "{err}");
    server.abort();
}

fn dev_source_for_port(port: u16, locs: Arc<MemLocations>) -> ImapSource {
    ImapSource::new(
        1,
        ImapServer {
            host: "127.0.0.1".into(),
            port,
            tls: ImapTls::Plain,
            username: "u".into(),
        },
        ImapPassword::new("pw"),
        locs,
        Arc::new(SourceRefusals::default()),
    )
    .unwrap()
    .with_timeouts(short_timeouts())
}

// ---- Amendment I: content-bound identity -----------------------------------

#[test]
fn the_header_digest_ignores_delivery_and_the_body_and_sees_the_headers() {
    // The self-sent pair: trace headers and address spelling differ, the
    // message does not.
    assert_eq!(header_digest(SENT_COPY), header_digest(INBOX_COPY));
    let a = mail(Some("same@x.org"), "hello");
    // The body is not identity (Amendment J): a rewritten body is the same
    // message as far as the id goes.
    let mut body_changed = a.clone();
    let n = body_changed.len();
    body_changed[n - 3] = b'X';
    assert_eq!(header_digest(&a), header_digest(&body_changed));
    // Reply-To is in the set: a spoof that only redirects replies differs.
    let with_reply_to = String::from_utf8(a.clone()).unwrap().replacen(
        "\r\n\r\n",
        "\r\nReply-To: evil@x.org\r\n\r\n",
        1,
    );
    assert_ne!(header_digest(&a), header_digest(with_reply_to.as_bytes()));
    // So are From and Subject.
    let from_changed =
        String::from_utf8(a.clone())
            .unwrap()
            .replacen("From: a@x.org", "From: b@x.org", 1);
    assert_ne!(header_digest(&a), header_digest(from_changed.as_bytes()));
    // ONE FUNCTION FOR EVERY PATH (review N6): the whole raw message and the
    // HEADER.FIELDS subset a fetch returns digest the same.
    let at = a.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
    assert_eq!(header_digest(&a), header_digest(&a[..at]));
    // A distinct id is never the known one, and is stable.
    let known = stable_id_for_message_id("same@x.org");
    let d = header_digest(with_reply_to.as_bytes());
    let other = distinct_stable_id(&known, Some("same@x.org"), &d);
    assert_ne!(other, known);
    assert!(other.starts_with(STABLE_ID_PREFIX));
    assert_eq!(other, distinct_stable_id(&known, Some("same@x.org"), &d));
    assert!(d.starts_with(DIGEST_PREFIX));
}

/// REVIEW N6: bare-LF headers over a CRLF body used to split at the body's
/// first CRLFCRLF, so the raw digest and the HEADER.FIELDS digest differed
/// and every MOVE of such a message read as a collision.
#[test]
fn the_header_digest_does_not_care_about_line_endings() {
    let lf_headers = b"From: a@x.org\nTo: me@dev.test\nSubject: s\n\
        Date: Mon, 26 Oct 2026 09:00:00 +0000\nMessage-ID: <eol@x.org>\n\n\
        para one\r\n\r\npara two\r\n";
    let crlf_subset = b"From: a@x.org\r\nTo: me@dev.test\r\nSubject: s\r\n\
        Date: Mon, 26 Oct 2026 09:00:00 +0000\r\n\r\n";
    assert_eq!(header_digest(lf_headers), header_digest(crlf_subset));
    // None of the fields at all: the subset reply ("\r\n") and the whole
    // message agree too.
    let bare = b"X-Thing: 1\r\n\r\nbody\r\n";
    assert_eq!(header_digest(bare), header_digest(b"\r\n"));
}

/// MATRIX 16b: the INBOX copy of a self-sent message re-encoded in transit
/// (quoted-printable where the Sent copy is 8bit) is the same message.
#[test]
fn a_reencoded_self_sent_copy_has_the_same_digest() {
    let sent = "From: me@dev.test\r\nTo: me@dev.test\r\nSubject: Cafe\r\n\
        Date: Mon, 26 Oct 2026 09:00:00 +0000\r\nMessage-ID: <s16b@x.org>\r\n\
        Content-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: 8bit\r\n\r\n\
        Caf\u{e9} cr\u{e8}me\r\n";
    let inbox = "Delivered-To: me@dev.test\r\nFrom: me@dev.test\r\nTo: me@dev.test\r\n\
        Subject: Cafe\r\nDate: Mon, 26 Oct 2026 09:00:00 +0000\r\nMessage-ID: <s16b@x.org>\r\n\
        Content-Type: text/plain; charset=utf-8\r\n\
        Content-Transfer-Encoding: quoted-printable\r\n\r\nCaf=C3=A9 cr=C3=A8me\r\n";
    assert_eq!(
        header_digest(sent.as_bytes()),
        header_digest(inbox.as_bytes())
    );
}

/// The Message-ID `< a@b >` (and one folded mid-id) is the id of `<a@b>`.
#[test]
fn whitespace_inside_the_brackets_is_not_part_of_the_id() {
    assert_eq!(
        normalize_message_id("< a@B.org >").as_deref(),
        Some("a@b.org")
    );
    assert_eq!(
        normalize_message_id("<abc\r\n def@x.org>").as_deref(),
        Some("abcdef@x.org")
    );
    let (id, _) = stable_id_for_header_block(b"Message-ID: < sp@x.org >\r\n\r\n");
    assert_eq!(id, stable_id_for_message_id("sp@x.org"));
}

fn spoof(mid: &str) -> Vec<u8> {
    format!(
        "From: mallory@evil.example\r\nTo: me@dev.test\r\nSubject: URGENT wire\r\n\
         Date: Tue, 27 Oct 2026 09:00:00 +0000\r\nMessage-ID: <{mid}>\r\n\r\nsend money\r\n"
    )
    .into_bytes()
}

fn raw_of(m: &FetchedMessage) -> Vec<u8> {
    match &m.raw {
        RawBody::Bytes(b) => b.clone(),
        other => panic!("no bytes: {}", matches!(other, RawBody::Missing)),
    }
}

/// A SPOOF REUSING A KNOWN MESSAGE-ID IS ANOTHER MESSAGE (Amendment I):
/// whether or not the original's body was read yet, the newcomer gets its own
/// id, and the original id still fetches the original's bytes only, with
/// placement from its own copy.
#[tokio::test]
#[ignore = "needs the Dovecot test bed: scripts/imap-dev.sh start"]
async fn live_a_spoof_with_a_known_message_id_gets_its_own_id() {
    for read_first in [true, false] {
        let user = fresh_user("spoof");
        let mut other = other_client(&user).await;
        let real = mail(Some("bank-1@bank.example"), "Your statement");
        append(&mut other, INBOX, &real, None).await;
        let (src, locs) = live(&user);
        let ids = src
            .list(inbox_listing(), None, Urgency::Live)
            .await
            .unwrap();
        let known = stable_id_for_message_id("bank-1@bank.example");
        assert_eq!(ids, vec![known.clone()]);
        if read_first {
            assert_eq!(
                raw_of(&src.fetch_raw(&known, Urgency::Live).await.unwrap()),
                real
            );
        }
        let cursor = src.current_cursor().await.unwrap();

        append(&mut other, INBOX, &spoof("bank-1@bank.example"), None).await;
        let walk = src.changes(&cursor, Folder::Inbox).await.unwrap();
        assert_eq!(walk.ids.len(), 1);
        let newcomer = walk.ids[0].clone();
        assert_ne!(newcomer, known, "read_first={read_first}");

        let m = src.fetch_raw(&known, Urgency::Live).await.unwrap();
        assert_eq!(raw_of(&m), real, "read_first={read_first}");
        let rows = locs.message_locations(1, &known).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].uid, 1);
        let s = src.fetch_raw(&newcomer, Urgency::Live).await.unwrap();
        assert_eq!(raw_of(&s), spoof("bank-1@bank.example"));
        // Seen again (a re-listing), it is the same newcomer id.
        let again = src
            .list(inbox_listing(), None, Urgency::Live)
            .await
            .unwrap();
        let mut again = again;
        again.sort();
        let mut want = vec![known.clone(), newcomer.clone()];
        want.sort();
        assert_eq!(again, want);

        // The real one goes stale (moved off to a user folder); the spoof in
        // INBOX shares its Message-ID exactly but not its content, so the
        // fallback finds the real one where it went, never the spoof.
        other.create("Statements").await.unwrap();
        other.select(INBOX).await.unwrap();
        other.uid_mv("1", "Statements").await.unwrap();
        let m = src.fetch_raw(&known, Urgency::Live).await.unwrap();
        assert_eq!(raw_of(&m), real);
        assert_eq!(m.placement, Placement::default());
        let rows = locs.message_locations(1, &known).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].folder, "Statements");
        let _ = other.logout().await;
    }
}

/// A MOVE INTO JUNK KEEPS THE ID: the Junk listing re-sights it, the digest
/// matches, and it is the same message at a new place.
#[tokio::test]
#[ignore = "needs the Dovecot test bed: scripts/imap-dev.sh start"]
async fn live_a_move_into_junk_keeps_the_id() {
    let user = fresh_user("junk");
    let mut other = other_client(&user).await;
    append(
        &mut other,
        INBOX,
        &mail(Some("j@x.org"), "maybe spam"),
        None,
    )
    .await;
    let (src, locs) = live(&user);
    let id = stable_id_for_message_id("j@x.org");
    assert_eq!(
        src.list(inbox_listing(), None, Urgency::Live)
            .await
            .unwrap(),
        vec![id.clone()]
    );
    // The engine reads each listed body: that binds the digest, and the
    // Junk sighting below is checked against it.
    src.fetch_raw(&id, Urgency::Live).await.unwrap();
    assert!(locs.content_digest(1, &id).unwrap().is_some());
    other.select(INBOX).await.unwrap();
    other.uid_mv("1", "Junk").await.unwrap();
    let junk = src
        .list(
            Listing::Folder {
                folder: Folder::Spam,
                within_days: None,
            },
            None,
            Urgency::Live,
        )
        .await
        .unwrap();
    assert_eq!(junk, vec![id.clone()]);
    let m = src.fetch_raw(&id, Urgency::Live).await.unwrap();
    assert!(matches!(m.raw, RawBody::Bytes(_)));
    let rows = locs.message_locations(1, &id).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].folder, "Junk");
    let _ = other.logout().await;
}

/// A LOOKALIKE NEVER STANDS IN (security review 2): `SEARCH HEADER` is a
/// substring match, so `<xabc@…>` and `<abc@….evil>` contain `abc@…`. With
/// the real message gone, the lookup for `abc@…` finds nothing rather than
/// one of them, even before any digest was recorded.
#[tokio::test]
#[ignore = "needs the Dovecot test bed: scripts/imap-dev.sh start"]
async fn live_a_lookalike_message_id_never_satisfies_a_lookup() {
    let user = fresh_user("lookalike");
    let mut other = other_client(&user).await;
    append(&mut other, INBOX, &mail(Some("abc@x.org"), "real"), None).await;
    let (src, locs) = live(&user);
    let id = stable_id_for_message_id("abc@x.org");
    src.list(inbox_listing(), None, Urgency::Live)
        .await
        .unwrap();
    // Gone (expunged), then lookalikes arrive.
    other.select(INBOX).await.unwrap();
    other
        .uid_store("1", "+FLAGS (\\Deleted)")
        .await
        .unwrap()
        .try_collect::<Vec<_>>()
        .await
        .unwrap();
    other
        .expunge()
        .await
        .unwrap()
        .try_collect::<Vec<_>>()
        .await
        .unwrap();
    append(
        &mut other,
        INBOX,
        &mail(Some("xabc@x.org"), "lookalike"),
        None,
    )
    .await;
    append(
        &mut other,
        INBOX,
        &mail(Some("abc@x.org.evil"), "lookalike 2"),
        None,
    )
    .await;
    let m = src.fetch_raw(&id, Urgency::Live).await.unwrap();
    assert!(matches!(m.raw, RawBody::Missing));
    assert!(locs.message_locations(1, &id).unwrap().is_empty());
    let _ = other.logout().await;
}

// ---- the credential slot (security review 5) -------------------------------

/// The slot carries the server and the CA's bytes with the password, reads
/// back whole, and never shows the password in `Debug`; a bare password (an
/// earlier build's slot) reads as the legacy shape, never as a config.
#[test]
fn the_credential_slot_holds_where_the_password_goes() {
    let c = ImapCredential {
        config: ImapAccountConfig {
            server: server("imap.example.org", ImapTls::Implicit),
            ca_pem: Some("-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n".into()),
        },
        password: ImapPassword::new("s3cret-app-pw"),
    };
    assert!(!format!("{c:?}").contains("s3cret"));
    let text = c.to_slot().unwrap();
    match ImapCredential::from_slot(&text) {
        StoredImapSlot::Credential(back) => {
            assert_eq!(back.config, c.config);
            assert_eq!(back.password.0, "s3cret-app-pw");
        }
        StoredImapSlot::LegacyPassword(_) => panic!("read as legacy"),
    }
    assert!(matches!(
        ImapCredential::from_slot("just-a-password"),
        StoredImapSlot::LegacyPassword(p) if p.0 == "just-a-password"
    ));
    // A CA that is not a certificate is refused at setup, by its bytes.
    assert!(parse_ca_pem(b"not a pem").is_err());
}

// ---- fake IDLE servers: keepalives, wake floods, the gaps, backoff --------

/// What a fake IDLE server does once a client is idling.
#[derive(Clone, Copy)]
enum Idling {
    /// `* OK Still here` every interval, and nothing else.
    Keepalive(Duration),
    /// A flag change (`* 1 FETCH`) the moment IDLE starts, every time.
    FetchFlood,
    /// `* 1 EXISTS` BEFORE the `+ idling` continuation, then quiet.
    ExistsBeforeContinue,
    /// Quiet, then `* 1 EXISTS` while answering `DONE`.
    ExistsInDone,
    /// `* 0 RECENT` (an untagged line that is not a change) every interval.
    Chatter(Duration),
}

/// Counts a fake IDLE server keeps.
#[derive(Default)]
struct IdleCounts {
    connections: AtomicU64,
    idles: AtomicU64,
}

/// A loopback server that logs anyone in (or refuses every LOGIN with
/// `refuse_login`), advertises IDLE, answers EXAMINE INBOX with an empty
/// mailbox, and behaves as `idling` says under IDLE.
async fn idle_server(
    idling: Idling,
    refuse_login: bool,
) -> (u16, Arc<IdleCounts>, tokio::task::JoinHandle<()>) {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let counts = Arc::new(IdleCounts::default());
    let c = counts.clone();
    let task = tokio::spawn(async move {
        loop {
            let Ok((sock, _)) = listener.accept().await else {
                return;
            };
            c.connections.fetch_add(1, Ordering::Relaxed);
            let c = c.clone();
            tokio::spawn(async move {
                let (r, mut w) = sock.into_split();
                let mut lines = BufReader::new(r).lines();
                let _ = w.write_all(b"* OK fake idle\r\n").await;
                let mut idle_tag: Option<String> = None;
                loop {
                    let line = if idle_tag.is_some() {
                        if let Idling::Keepalive(every) | Idling::Chatter(every) = idling {
                            let say: &[u8] = if matches!(idling, Idling::Chatter(_)) {
                                b"* 0 RECENT\r\n"
                            } else {
                                b"* OK Still here\r\n"
                            };
                            tokio::select! {
                                l = lines.next_line() => l,
                                _ = tokio::time::sleep(every) => {
                                    if w.write_all(say).await.is_err() {
                                        return;
                                    }
                                    continue;
                                }
                            }
                        } else {
                            lines.next_line().await
                        }
                    } else {
                        lines.next_line().await
                    };
                    let Ok(Some(line)) = line else { return };
                    let reply = if let Some(tag) = idle_tag.take() {
                        // Under IDLE the only thing a client says is DONE.
                        assert_eq!(line.trim(), "DONE");
                        match idling {
                            Idling::ExistsInDone => format!("* 1 EXISTS\r\n{tag} OK done\r\n"),
                            _ => format!("{tag} OK done\r\n"),
                        }
                    } else {
                        let (tag, cmd) = line.split_once(' ').unwrap_or((line.as_str(), ""));
                        let cmd = cmd.to_ascii_uppercase();
                        if cmd.starts_with("LOGIN") && refuse_login {
                            format!("{tag} NO [AUTHENTICATIONFAILED] nope\r\n")
                        } else if cmd.starts_with("CAPABILITY") {
                            format!("* CAPABILITY IMAP4rev1 IDLE\r\n{tag} OK done\r\n")
                        } else if cmd.starts_with("EXAMINE") {
                            format!(
                                "* 0 EXISTS\r\n* OK [UIDVALIDITY 7] ok\r\n* OK [UIDNEXT 1] ok\r\n\
                                 {tag} OK [READ-ONLY] done\r\n"
                            )
                        } else if cmd.starts_with("IDLE") {
                            c.idles.fetch_add(1, Ordering::Relaxed);
                            idle_tag = Some(tag.to_string());
                            match idling {
                                Idling::FetchFlood => {
                                    "+ idling\r\n* 1 FETCH (FLAGS (\\Seen))\r\n".to_string()
                                }
                                Idling::ExistsBeforeContinue => {
                                    "* 1 EXISTS\r\n+ idling\r\n".to_string()
                                }
                                _ => "+ idling\r\n".to_string(),
                            }
                        } else {
                            format!("{tag} OK done\r\n")
                        }
                    };
                    if w.write_all(reply.as_bytes()).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    (port, counts, task)
}

/// CORRECTNESS H1: async-imap's IDLE timer restarts on every server line, so
/// with keepalives flowing a round used to run until the outer bound
/// (deadline + IDLE_GRACE) cut it and dropped the connection. The round now
/// ends on its own clock, `DONE`s cleanly and parks the session.
#[tokio::test]
async fn keepalives_neither_stretch_the_wait_nor_count_as_a_wake() {
    let (port, counts, server) =
        idle_server(Idling::Keepalive(Duration::from_millis(150)), false).await;
    let src = scripted_source(port, "pw");
    for _ in 0..2 {
        let t = tokio::time::Instant::now();
        src.wait_for_change(None, Duration::from_millis(1500)).await;
        let took = t.elapsed();
        assert!(took >= Duration::from_millis(1450), "not a wake: {took:?}");
        assert!(took < Duration::from_millis(2500), "on time: {took:?}");
    }
    // Parked after a clean DONE and reused: one connection, one login.
    assert_eq!(counts.connections.load(Ordering::Relaxed), 1);
    assert_eq!(src.idle_connects(), 1);
    server.abort();
}

/// The re-issue happens under keepalives too: a round ends at
/// `idle_reissue` and a fresh IDLE starts, on the same connection.
#[tokio::test]
async fn idle_is_reissued_on_schedule_while_keepalives_flow() {
    let (port, counts, server) =
        idle_server(Idling::Keepalive(Duration::from_millis(50)), false).await;
    let src = scripted_source(port, "pw").with_timeouts(Timeouts {
        idle_reissue: Duration::from_millis(250),
        ..Timeouts::default()
    });
    src.wait_for_change(None, Duration::from_millis(1400)).await;
    let idles = counts.idles.load(Ordering::Relaxed);
    assert!((4..=7).contains(&idles), "{idles} IDLE rounds");
    assert_eq!(counts.connections.load(Ordering::Relaxed), 1);
    server.abort();
}

/// SECURITY REVIEW 8: a server streaming FETCH under IDLE wakes every wait
/// at once, but no two early returns come closer than MIN_WAKE_GAP.
#[tokio::test]
async fn a_wake_flood_is_spaced_out() {
    let (port, _, server) = idle_server(Idling::FetchFlood, false).await;
    let src = scripted_source(port, "pw");
    let t = tokio::time::Instant::now();
    src.wait_for_change(None, Duration::from_secs(30)).await;
    assert!(
        t.elapsed() < Duration::from_secs(1),
        "the first wake is not held"
    );
    for _ in 0..2 {
        src.wait_for_change(None, Duration::from_secs(30)).await;
    }
    let took = t.elapsed();
    assert!(
        took >= MIN_WAKE_GAP * 2 - Duration::from_millis(50),
        "{took:?}"
    );
    assert!(took < MIN_WAKE_GAP * 2 + Duration::from_secs(1), "{took:?}");
    // And never past the wait's own deadline.
    let t = tokio::time::Instant::now();
    src.wait_for_change(None, Duration::from_millis(500)).await;
    assert!(
        t.elapsed() < Duration::from_millis(900),
        "{:?}",
        t.elapsed()
    );
    server.abort();
}

/// CORRECTNESS L2: an EXISTS sent between EXAMINE and `+ idling`, or while
/// DONE is answered, lands in the unsolicited channel; it is a wake.
#[tokio::test]
async fn an_arrival_in_the_idle_gaps_is_a_wake() {
    let (port, _, server) = idle_server(Idling::ExistsBeforeContinue, false).await;
    let src = scripted_source(port, "pw");
    let t = tokio::time::Instant::now();
    src.wait_for_change(None, Duration::from_secs(10)).await;
    assert!(t.elapsed() < Duration::from_secs(1), "{:?}", t.elapsed());
    server.abort();

    // Said during DONE at a re-issue: the wait returns then, not at its
    // deadline.
    let (port, _, server) = idle_server(Idling::ExistsInDone, false).await;
    let src = scripted_source(port, "pw").with_timeouts(Timeouts {
        idle_reissue: Duration::from_millis(300),
        ..Timeouts::default()
    });
    let t = tokio::time::Instant::now();
    src.wait_for_change(None, Duration::from_secs(10)).await;
    assert!(
        t.elapsed() < Duration::from_millis(1500),
        "{:?}",
        t.elapsed()
    );
    server.abort();
}

/// CORRECTNESS L3: refused IDLE logins back off within the wait (1s, 2s,
/// ...) and across waits, and the whole run counts ONE auth refusal.
#[tokio::test]
async fn refused_idle_logins_back_off_and_count_once() {
    let (port, counts, server) = idle_server(Idling::FetchFlood, true).await;
    let src = scripted_source(port, "wrong");
    let t = tokio::time::Instant::now();
    src.wait_for_change(None, Duration::from_secs(5)).await;
    assert!(t.elapsed() >= Duration::from_millis(4900), "slept out");
    // Attempts at about 0s, 1s and 3s; the next would be at 7s.
    let tries = counts.connections.load(Ordering::Relaxed);
    assert_eq!(tries, 3, "backed off, never spun");
    // The next wait starts inside the backoff: at most one more try.
    src.wait_for_change(None, Duration::from_secs(2)).await;
    assert!(counts.connections.load(Ordering::Relaxed) <= tries + 1);
    assert_eq!(src.refusals.auth.load(Ordering::Relaxed), 1);
    server.abort();
}

/// CORRECTNESS H2, the leg the bed cannot show (its folders are recreated
/// on access): Sent is deleted and STAYS gone, so STATUS and EXAMINE answer
/// NO and LIST no longer has it. The expiry's catch-up used to fail on the
/// session's cached roles ("a watched folder vanished mid-sync"), forever.
#[tokio::test]
async fn a_watched_folder_that_stays_gone_drops_out_of_the_cursor() {
    use std::sync::atomic::AtomicBool;
    let gone = Arc::new(AtomicBool::new(false));
    let lists = Arc::new(AtomicU64::new(0));
    let (g, l) = (gone.clone(), lists.clone());
    let (port, _, server) = scripted_server(true, move |tag, cmd| {
        let gone = g.load(Ordering::Relaxed);
        let sent = cmd.contains("\"SENT\"");
        let reply = if cmd.starts_with("LIST") {
            l.fetch_add(1, Ordering::Relaxed);
            let mut r = "* LIST () \"/\" INBOX\r\n".to_string();
            if !gone {
                r.push_str("* LIST (\\Sent) \"/\" Sent\r\n");
            }
            format!("{r}{tag} OK done\r\n")
        } else if cmd.starts_with("LOGIN") {
            format!("{tag} OK logged in\r\n")
        } else if cmd.starts_with("CAPABILITY") {
            format!("* CAPABILITY IMAP4rev1\r\n{tag} OK done\r\n")
        } else if (cmd.starts_with("STATUS") || cmd.starts_with("EXAMINE")) && sent && gone {
            format!("{tag} NO [NONEXISTENT] Mailbox doesn't exist: Sent\r\n")
        } else if cmd.starts_with("STATUS") {
            let (name, v) = if sent { ("Sent", 5) } else { ("INBOX", 7) };
            format!("* STATUS {name} (UIDVALIDITY {v} UIDNEXT 1)\r\n{tag} OK done\r\n")
        } else if cmd.starts_with("EXAMINE") {
            let v = if sent { 5 } else { 7 };
            format!(
                "* 0 EXISTS\r\n* OK [UIDVALIDITY {v}] ok\r\n* OK [UIDNEXT 1] ok\r\n\
                 {tag} OK [READ-ONLY] done\r\n"
            )
        } else if cmd.starts_with("UID SEARCH") {
            format!("* SEARCH\r\n{tag} OK done\r\n")
        } else {
            format!("{tag} OK done\r\n")
        };
        Say::Reply(reply)
    })
    .await;
    let src = scripted_source(port, "pw");
    let cursor = catch_up(&src).await;
    assert!(
        ImapCursor::decode(&cursor)
            .unwrap()
            .folders
            .contains_key("sent")
    );

    gone.store(true, Ordering::Relaxed);
    let cursor = converge(&src, cursor).await;
    let decoded = ImapCursor::decode(&cursor).unwrap();
    assert!(!decoded.folders.contains_key("sent"), "{decoded:?}");
    assert!(decoded.folders.contains_key("inbox"));
    // Settled: polling goes on, on the same session, without re-listing
    // folders every tick.
    let lists_then = lists.load(Ordering::Relaxed);
    for _ in 0..3 {
        tick(&src, &cursor).await.expect("no further catch-up");
    }
    assert_eq!(lists.load(Ordering::Relaxed), lists_then);
    assert_eq!(src.connects(), 1);
    server.abort();
}

// ---- fix round 3: a session the server hung up on ------------------------

/// A STATUS reply for the scripted servers below: INBOX at UIDVALIDITY 7,
/// UIDNEXT 3; Sent at 5, UIDNEXT 1.
fn status_reply(tag: &str, cmd: &str) -> String {
    let (name, v, next) = if cmd.contains("\"SENT\"") {
        ("Sent", 5, 1)
    } else {
        ("INBOX", 7, 3)
    };
    format!("* STATUS {name} (UIDVALIDITY {v} UIDNEXT {next})\r\n{tag} OK done\r\n")
}

/// MATRIX ROWS 11/18: a kicked command session sat in the pool with `* BYE`
/// and an EOF queued on it, and every later command was written into it and
/// "answered" with an empty STATUS, an error that did not retire it, for as
/// long as the engine kept retrying. Now the next call notices the hang-up
/// before it sends anything and logs in again at once: the call succeeds.
#[tokio::test]
async fn a_session_kicked_while_unused_is_replaced_before_the_next_call() {
    let statuses = Arc::new(AtomicU64::new(0));
    let st = statuses.clone();
    let (port, accepted, server) = scripted_server(true, move |tag, cmd| {
        if let Some(r) = login_ok(tag, cmd) {
            return Say::Reply(r);
        }
        if cmd.starts_with("STATUS") {
            // The first connection's last STATUS answer comes with a kick.
            let n = st.fetch_add(1, Ordering::Relaxed);
            let r = status_reply(tag, cmd);
            return if n == 1 {
                Say::HangUp(format!("{r}* BYE Disconnected for inactivity\r\n"))
            } else {
                Say::Reply(r)
            };
        }
        Say::Reply(format!("{tag} OK done\r\n"))
    })
    .await;
    let src = scripted_source(port, "pw");
    src.current_cursor().await.expect("first cursor");
    assert_eq!(src.connects(), 1);
    // Let the BYE and the FIN land.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let cursor = src
        .current_cursor()
        .await
        .expect("the dead session is replaced, not used");
    let decoded = ImapCursor::decode(&cursor).unwrap();
    assert_eq!(decoded.folders["inbox"].uidvalidity, 7);
    assert_eq!(src.connects(), 2);
    assert_eq!(accepted.load(Ordering::Relaxed), 2);
    server.abort();
}

/// A server that hangs up IN THE MIDDLE of a command (BYE, then EOF, no
/// tagged reply): async-imap hands back what it had parsed as Ok. An empty
/// SEARCH must not read as "no new mail" (the walk would step its cursor
/// over the mail it never saw), and the dead session must not be kept.
#[tokio::test]
async fn a_hang_up_mid_command_is_an_error_and_retires_the_session() {
    let searches = Arc::new(AtomicU64::new(0));
    let se = searches.clone();
    let (port, _, server) = scripted_server(true, move |tag, cmd| {
        if let Some(r) = login_ok(tag, cmd) {
            return Say::Reply(r);
        }
        if cmd.starts_with("STATUS") {
            return Say::Reply(status_reply(tag, cmd));
        }
        if cmd.starts_with("EXAMINE") {
            return Say::Reply(format!(
                "* 2 EXISTS\r\n* OK [UIDVALIDITY 7] ok\r\n* OK [UIDNEXT 3] ok\r\n\
                 {tag} OK [READ-ONLY] done\r\n"
            ));
        }
        if cmd.starts_with("UID SEARCH") {
            se.fetch_add(1, Ordering::Relaxed);
            return Say::HangUp("* BYE shutting down\r\n".into());
        }
        Say::Reply(format!("{tag} OK done\r\n"))
    })
    .await;
    let src = scripted_source(port, "pw");
    let start = ImapCursor {
        v: 1,
        folders: [("inbox".to_string(), pos("INBOX", 7, 0, None))]
            .into_iter()
            .collect(),
    }
    .encode();
    match src.changes(&start, Folder::Inbox).await {
        Err(ChangesError::Other(e)) => assert!(is_session_broken(&e), "{e}"),
        Ok(c) => panic!("a walk off a dead stream came back Ok: {:?}", c.ids),
        Err(ChangesError::Expired) => panic!("not a reset"),
    }
    assert_eq!(searches.load(Ordering::Relaxed), 1);
    // The next call is on a new login.
    src.current_cursor().await.expect("a fresh session");
    assert_eq!(src.connects(), 2);
    server.abort();
}

/// REVIEW N7: an untagged line that is not a change used to end the round:
/// DONE, then a fresh IDLE, per line, at whatever pace the server set. The
/// same IDLE now reads past it.
#[tokio::test]
async fn chatter_under_idle_does_not_cycle_done_and_idle() {
    let (port, counts, server) =
        idle_server(Idling::Chatter(Duration::from_millis(20)), false).await;
    let src = scripted_source(port, "pw");
    let t = tokio::time::Instant::now();
    src.wait_for_change(None, Duration::from_millis(1000)).await;
    let took = t.elapsed();
    assert!(took >= Duration::from_millis(950), "not a wake: {took:?}");
    let idles = counts.idles.load(Ordering::Relaxed);
    assert_eq!(idles, 1, "{idles} IDLE commands for ~50 chatter lines");
    server.abort();
}

/// A kicked command session (matrix row 18) costs one login inside the
/// next walk, not a failed walk: mail delivered after the kick is in it.
#[tokio::test]
#[ignore = "needs the Dovecot test bed: scripts/imap-dev.sh start"]
async fn live_a_kicked_command_session_is_replaced_inside_the_next_walk() {
    let user = fresh_user("kickcmd");
    let mut other = other_client(&user).await;
    append(&mut other, INBOX, &mail(Some("pre@x.org"), "pre"), None).await;
    let (src, _) = live(&user);
    let cursor = catch_up(&src).await;
    assert_eq!(src.connects(), 1);
    kick(&user);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let mut other = other_client(&user).await;
    append(&mut other, INBOX, &mail(Some("kicked1@x.org"), "k"), None).await;
    let t = std::time::Instant::now();
    let walk = src
        .changes(&cursor, Folder::Inbox)
        .await
        .expect("the walk logs in again instead of failing");
    assert_eq!(walk.ids, vec![stable_id_for_message_id("kicked1@x.org")]);
    assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
    assert_eq!(src.connects(), 2);
    let _ = other.logout().await;
}

// ---- fix round 3: Amendment J (first-seen wins, header-only digest) -------

/// A multipart whose declared boundary and References are the knobs.
fn multipart(boundary: &str, references: &str) -> Vec<u8> {
    format!(
        "From: alice@x.org\r\nTo: me@dev.test\r\nSubject: Invoice\r\n\
         Date: Mon, 26 Oct 2026 09:00:00 +0000\r\nMessage-ID: <n1@x.org>\r\n\
         References: {references}\r\nMIME-Version: 1.0\r\n\
         Content-Type: multipart/alternative; boundary=\"{boundary}\"\r\n\r\n\
         preamble\r\n--B\r\nContent-Type: text/plain\r\n\r\nPAY NOW to evil\r\n\
         --A\r\nContent-Type: text/plain\r\n\r\nyour invoice is attached\r\n--A--\r\n--B--\r\n"
    )
    .into_bytes()
}

/// REVIEW N1: the original sender re-sends the message with identical digest
/// fields and identical body text, but a different declared MIME boundary
/// (so the body now SAYS something else) and different References (so it
/// would re-key the thread). Same id, and under first-seen-wins the
/// original's location stands: every fetch of the id reads the ORIGINAL
/// bytes, so the stored row, its body and its thread are untouched.
#[tokio::test]
#[ignore = "needs the Dovecot test bed: scripts/imap-dev.sh start"]
async fn live_a_resend_under_a_known_id_never_replaces_the_original_bytes() {
    let user = fresh_user("resend");
    let mut other = other_client(&user).await;
    let original = multipart("A", "<root@x.org>");
    append(&mut other, INBOX, &original, None).await;
    let (src, locs) = live(&user);
    let id = stable_id_for_message_id("n1@x.org");
    assert_eq!(
        src.list(inbox_listing(), None, Urgency::Live)
            .await
            .unwrap(),
        vec![id.clone()]
    );
    let first = src.fetch_raw(&id, Urgency::Live).await.unwrap();
    assert_eq!(raw_of(&first), original);
    let cursor = src.current_cursor().await.unwrap();

    let resend = multipart("B", "<other-thread@evil.example>");
    append(&mut other, INBOX, &resend, None).await;
    let walk = src.changes(&cursor, Folder::Inbox).await.unwrap();
    // The same message as far as identity goes ...
    assert_eq!(walk.ids, vec![id.clone()]);
    // ... and its re-ingest reads the first copy, whatever the second holds.
    let m = src.fetch_raw(&id, Urgency::Live).await.unwrap();
    assert_eq!(raw_of(&m), original);
    assert_eq!(m.thread_id, None);
    let rows = locs.message_locations(1, &id).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!((rows[0].folder.as_str(), rows[0].uid), (INBOX, 1));
    // A re-listing of the whole folder does not move it either.
    src.list(inbox_listing(), None, Urgency::Live)
        .await
        .unwrap();
    let m = src.fetch_raw(&id, Urgency::Live).await.unwrap();
    assert_eq!(raw_of(&m), original);
    let _ = other.logout().await;
}

/// MATRIX 16b: a self-sent message whose INBOX copy was re-encoded in transit
/// (quoted-printable; the Sent copy is 8bit) is ONE id with both locations.
#[tokio::test]
#[ignore = "needs the Dovecot test bed: scripts/imap-dev.sh start"]
async fn live_a_reencoded_self_sent_copy_is_one_id() {
    let user = fresh_user("cte");
    let mut other = other_client(&user).await;
    let sent = "From: me@dev.test\r\nTo: me@dev.test\r\nSubject: Cafe creme\r\n\
        Date: Mon, 26 Oct 2026 09:00:00 +0000\r\nMessage-ID: <s16b@dev.test>\r\n\
        MIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\n\
        Content-Transfer-Encoding: 8bit\r\n\r\nCaf\u{e9} cr\u{e8}me\r\n";
    let inbox = "Return-Path: <me@dev.test>\r\nDelivered-To: me@dev.test\r\n\
        From: me@dev.test\r\nTo: me@dev.test\r\nSubject: Cafe creme\r\n\
        Date: Mon, 26 Oct 2026 09:00:00 +0000\r\nMessage-ID: <s16b@dev.test>\r\n\
        MIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\n\
        Content-Transfer-Encoding: quoted-printable\r\n\r\nCaf=C3=A9 cr=C3=A8me\r\n";
    let (src, locs) = live(&user);
    let start = src.current_cursor().await.unwrap();
    append(&mut other, "Sent", sent.as_bytes(), None).await;
    append(&mut other, INBOX, inbox.as_bytes(), None).await;
    let s = src.changes(&start, Folder::Sent).await.unwrap();
    let i = src.changes(&start, Folder::Inbox).await.unwrap();
    let id = stable_id_for_message_id("s16b@dev.test");
    assert_eq!(s.ids, vec![id.clone()]);
    assert_eq!(i.ids, vec![id.clone()]);
    let rows = locs.message_locations(1, &id).unwrap();
    assert_eq!(rows.len(), 2, "{rows:?}");
    let m = src.fetch_raw(&id, Urgency::Live).await.unwrap();
    assert!(m.placement.in_inbox && m.placement.in_sent);
    // First seen first: the Sent copy is the bytes.
    assert_eq!(raw_of(&m), sent.as_bytes());
    let _ = other.logout().await;
}

/// MATRIX 15: a spoof that arrives FIRST owns the plain id, so the real
/// message gets the distinct id, and with it its own thread: before, both
/// threaded on the shared Message-ID, so the real bank mail sat inside the
/// attacker's thread under the attacker's subject.
#[tokio::test]
#[ignore = "needs the Dovecot test bed: scripts/imap-dev.sh start"]
async fn live_the_later_of_two_messages_sharing_a_message_id_is_its_own_thread() {
    let user = fresh_user("spooffirst");
    let mut other = other_client(&user).await;
    let (src, _) = live(&user);
    let start = src.current_cursor().await.unwrap();
    append(&mut other, INBOX, &spoof("sf-1@bank.example"), None).await;
    let w1 = src.changes(&start, Folder::Inbox).await.unwrap();
    let plain = stable_id_for_message_id("sf-1@bank.example");
    assert_eq!(w1.ids, vec![plain.clone()]);
    let real = mail(Some("sf-1@bank.example"), "Your statement");
    append(&mut other, INBOX, &real, None).await;
    let w2 = src.changes(&w1.cursor, Folder::Inbox).await.unwrap();
    assert_eq!(w2.ids.len(), 1);
    let distinct = w2.ids[0].clone();
    assert_ne!(distinct, plain);
    let m = src.fetch_raw(&distinct, Urgency::Live).await.unwrap();
    assert_eq!(raw_of(&m), real);
    assert_eq!(m.thread_id.as_deref(), Some(distinct.as_str()));
    // The first-seen one is ordinary mail: threaded by ingest.
    let s = src.fetch_raw(&plain, Urgency::Live).await.unwrap();
    assert_eq!(raw_of(&s), spoof("sf-1@bank.example"));
    assert_eq!(s.thread_id, None);
    let _ = other.logout().await;
}

/// MATRIX 20: a message over the ingest bound is stepped over: the walk
/// lists it, its fetch says Undecodable (never downloaded, never an error),
/// and the mail behind it arrives.
#[tokio::test]
#[ignore = "needs the Dovecot test bed: scripts/imap-dev.sh start"]
async fn live_an_oversized_message_does_not_block_the_mail_behind_it() {
    let user = fresh_user("big");
    let mut other = other_client(&user).await;
    let (src, _) = live(&user);
    let start = src.current_cursor().await.unwrap();
    let mut big = mail(Some("big@x.org"), "big").to_vec();
    big.truncate(big.len() - 2);
    let line = [b'A'; 998];
    while big.len() < (MAX_RAW_BYTES as usize) + 1024 * 1024 {
        big.extend_from_slice(&line);
        big.extend_from_slice(b"\r\n");
    }
    append(&mut other, INBOX, &big, None).await;
    drop(big);
    append(&mut other, INBOX, &mail(Some("after@x.org"), "after"), None).await;
    let w = src.changes(&start, Folder::Inbox).await.unwrap();
    let (big_id, after_id) = (
        stable_id_for_message_id("big@x.org"),
        stable_id_for_message_id("after@x.org"),
    );
    let mut want = vec![big_id.clone(), after_id.clone()];
    want.sort();
    assert_eq!(w.ids, want);
    let m = src.fetch_raw(&big_id, Urgency::Live).await.unwrap();
    assert!(
        matches!(&m.raw, RawBody::Undecodable(why) if why.contains("64MB")),
        "an oversized message is skipped"
    );
    let a = src.fetch_raw(&after_id, Urgency::Live).await.unwrap();
    assert!(matches!(a.raw, RawBody::Bytes(_)));
    let _ = other.logout().await;
}

// ---- fix round 3: roles (review N4, N5) -----------------------------------

#[test]
fn a_folder_that_cannot_be_opened_never_holds_a_role() {
    let roles = resolve_roles(&names(&[
        ("INBOX", &[]),
        ("Sent", &["\\Noselect", "\\Sent"]),
        ("Sent Items", &[]),
        ("Junk", &["\\NonExistent"]),
    ]));
    assert_eq!(roles.sent.as_deref(), Some("Sent Items"));
    assert_eq!(roles.junk, None);
    assert!(!roles.selectable.contains(&"Sent".to_string()));
}

/// A server for the role tests: LIST names Sent when `listed`, STATUS on
/// Sent answers NO when `refuse`.
async fn role_server(
    listed: Arc<std::sync::atomic::AtomicBool>,
    refuse: Arc<std::sync::atomic::AtomicBool>,
) -> (u16, tokio::task::JoinHandle<()>) {
    let (port, _, server) = scripted_server(true, move |tag, cmd| {
        let sent = cmd.contains("\"SENT\"");
        let reply = if cmd.starts_with("LIST") {
            let mut r = "* LIST () \"/\" INBOX\r\n".to_string();
            if listed.load(Ordering::Relaxed) {
                r.push_str("* LIST (\\Sent) \"/\" Sent\r\n");
            }
            format!("{r}{tag} OK done\r\n")
        } else if let Some(r) = login_ok(tag, cmd) {
            r
        } else if cmd.starts_with("STATUS") && sent && refuse.load(Ordering::Relaxed) {
            format!("{tag} NO [UNAVAILABLE] try later\r\n")
        } else if cmd.starts_with("STATUS") {
            status_reply(tag, cmd)
        } else if cmd.starts_with("EXAMINE") {
            let v = if sent { 5 } else { 7 };
            format!(
                "* 0 EXISTS\r\n* OK [UIDVALIDITY {v}] ok\r\n* OK [UIDNEXT 1] ok\r\n\
                 {tag} OK [READ-ONLY] done\r\n"
            )
        } else if cmd.starts_with("UID SEARCH") {
            format!("* SEARCH\r\n{tag} OK done\r\n")
        } else {
            format!("{tag} OK done\r\n")
        };
        Say::Reply(reply)
    })
    .await;
    (port, server)
}

/// REVIEW N4: a role folder LIST still names but that refuses STATUS (a
/// lock timeout, UNAVAILABLE) was read as gone: the cursor lost its entry
/// and the next walk sent the engine to a full catch-up, every tick for a
/// persistent refusal. It is a retryable error now; after a few in a row it
/// is left unwatched, without a catch-up.
#[tokio::test]
async fn a_listed_folder_that_refuses_status_is_retried_not_dropped() {
    use std::sync::atomic::AtomicBool;
    let (listed, refuse) = (
        Arc::new(AtomicBool::new(true)),
        Arc::new(AtomicBool::new(false)),
    );
    let (port, server) = role_server(listed.clone(), refuse.clone()).await;
    let src = scripted_source(port, "pw");
    let cursor = catch_up(&src).await;
    refuse.store(true, Ordering::Relaxed);
    for attempt in 1..REFUSALS_BEFORE_ABSENT {
        match src.changes(&cursor, Folder::Sent).await {
            Err(ChangesError::Other(e)) => assert!(!is_session_broken(&e), "{e}"),
            Err(ChangesError::Expired) => panic!("refusal {attempt} read as a reset"),
            Ok(c) => panic!("refusal {attempt} read as success: {:?}", c.ids),
        }
    }
    // Persistent: unwatched for the session, the walk goes on, no Expired.
    let w = src.changes(&cursor, Folder::Sent).await.expect("given up");
    assert!(
        !ImapCursor::decode(&w.cursor)
            .unwrap()
            .folders
            .contains_key("sent")
    );
    // A transient one that clears is just a retry.
    let src2 = scripted_source(port, "pw");
    refuse.store(false, Ordering::Relaxed);
    let cursor = catch_up(&src2).await;
    refuse.store(true, Ordering::Relaxed);
    assert!(matches!(
        src2.changes(&cursor, Folder::Sent).await,
        Err(ChangesError::Other(_))
    ));
    refuse.store(false, Ordering::Relaxed);
    let w = src2.changes(&cursor, Folder::Sent).await.unwrap();
    assert_eq!(
        ImapCursor::decode(&w.cursor).unwrap().folders["sent"].uidvalidity,
        5
    );
    assert_eq!(src.connects(), 1);
    server.abort();
}

/// REVIEW N5: a role folder that appears (recreated in another client) after
/// login was invisible until the session dropped, which poll traffic never
/// lets happen. The roles are listed again periodically.
#[tokio::test]
async fn a_role_folder_that_appears_later_is_picked_up_without_a_new_login() {
    use std::sync::atomic::AtomicBool;
    let (listed, refuse) = (
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
    );
    let (port, server) = role_server(listed.clone(), refuse).await;
    let src = scripted_source(port, "pw").with_timeouts(Timeouts {
        roles_relist: Duration::from_millis(200),
        ..short_timeouts()
    });
    let cursor = src.current_cursor().await.unwrap();
    assert!(
        !ImapCursor::decode(&cursor)
            .unwrap()
            .folders
            .contains_key("sent")
    );
    listed.store(true, Ordering::Relaxed);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let w = src.changes(&cursor, Folder::Sent).await.unwrap();
    assert_eq!(
        ImapCursor::decode(&w.cursor).unwrap().folders["sent"].mailbox,
        "Sent"
    );
    assert_eq!(src.connects(), 1);
    server.abort();
}
