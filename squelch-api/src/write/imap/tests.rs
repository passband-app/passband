use super::*;

use std::collections::HashMap;
use std::sync::Mutex;

use squelch_core::mail::imap::{
    BodyPrint, ImapAccountConfig, ImapPassword, ImapServer, ImapTls, MessageLocations, SmtpServer,
};

// ---- look, then APPEND: the decision -----------------------------------------

/// A Sent folder that answers from a script: the look that first finds the
/// message (`None`: the server never saves one), looks that fail, and every
/// APPEND it is asked for.
struct ScriptedSent {
    found_at_look: Option<usize>,
    failing_looks: usize,
    looks: usize,
    appended: Vec<Vec<u8>>,
}

impl ScriptedSent {
    fn new(found_at_look: Option<usize>) -> Self {
        Self {
            found_at_look,
            failing_looks: 0,
            looks: 0,
            appended: Vec::new(),
        }
    }
}

fn loc(uid: u32) -> MessageLocation {
    MessageLocation {
        folder: "Sent".into(),
        uidvalidity: 7,
        uid,
        message_id: Some("m@x".into()),
    }
}

#[async_trait::async_trait]
impl SentFolder for ScriptedSent {
    async fn look(&mut self, _: &str) -> Result<Option<MessageLocation>, String> {
        self.looks += 1;
        if self.looks <= self.failing_looks {
            return Err("connection reset".into());
        }
        if !self.appended.is_empty() {
            return Ok(Some(loc(99)));
        }
        Ok(self
            .found_at_look
            .filter(|n| self.looks >= *n)
            .map(|_| loc(5)))
    }
    async fn append_seen(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.appended.push(bytes.to_vec());
        Ok(())
    }
}

const QUICK: [Duration; 4] = [
    Duration::ZERO,
    Duration::from_millis(1),
    Duration::from_millis(1),
    Duration::from_millis(1),
];

/// A server that files its own copy (on the third look here) gets no second
/// one from us.
#[tokio::test]
async fn a_server_that_saved_its_own_copy_gets_no_append() {
    let mut sent = ScriptedSent::new(Some(3));
    let got = look_then_append(&mut sent, "m@x", b"saved", &QUICK)
        .await
        .unwrap();
    assert_eq!(got, SentCopy::ServerSaved(loc(5)));
    assert!(sent.appended.is_empty());
    assert_eq!(sent.looks, 3, "stops looking once found");
}

/// A server that never saves one gets exactly one APPEND of the saved copy
/// (Bcc and all), after every look, and the location is read back.
#[tokio::test]
async fn a_server_that_saved_nothing_gets_exactly_one_append() {
    let mut sent = ScriptedSent::new(None);
    let got = look_then_append(&mut sent, "m@x", b"Bcc: h@y\r\n\r\nb", &QUICK)
        .await
        .unwrap();
    assert_eq!(got, SentCopy::Appended(Some(loc(99))));
    assert_eq!(sent.appended, vec![b"Bcc: h@y\r\n\r\nb".to_vec()]);
    assert_eq!(sent.looks, QUICK.len() + 1);
}

/// Looks that fail are not proof of a copy: Sent still gets one.
#[tokio::test]
async fn failing_looks_still_end_in_one_append() {
    let mut sent = ScriptedSent::new(None);
    sent.failing_looks = QUICK.len();
    let got = look_then_append(&mut sent, "m@x", b"x", &QUICK)
        .await
        .unwrap();
    assert!(matches!(got, SentCopy::Appended(_)));
    assert_eq!(sent.appended.len(), 1);
}

#[test]
fn smtp_failures_keep_the_maybe_sent_distinction() {
    assert!(matches!(
        smtp_write_error(SmtpError::MaybeSent("x".into())),
        WriteError::Transport(_)
    ));
    assert!(matches!(
        smtp_write_error(SmtpError::Refused("x".into())),
        WriteError::Api { status: 400, .. }
    ));
    assert!(matches!(
        smtp_write_error(SmtpError::NotSent("x".into())),
        WriteError::Api { status: 503, .. }
    ));
    assert!(matches!(
        smtp_write_error(SmtpError::Auth),
        WriteError::Api { status: 401, .. }
    ));
}

// ---- the writer ----------------------------------------------------------------

#[derive(Default)]
struct MemLocations {
    rows: Mutex<HashMap<String, Vec<MessageLocation>>>,
    digests: Mutex<HashMap<String, String>>,
    prints: Mutex<HashMap<String, BodyPrint>>,
}

impl MessageLocations for MemLocations {
    fn message_locations(
        &self,
        _: squelch_core::types::AccountId,
        msg_id: &str,
    ) -> squelch_core::Result<Vec<MessageLocation>> {
        Ok(self
            .rows
            .lock()
            .unwrap()
            .get(msg_id)
            .cloned()
            .unwrap_or_default())
    }
    fn record_message_locations(
        &self,
        _: squelch_core::types::AccountId,
        rows: &[(String, MessageLocation)],
    ) -> squelch_core::Result<()> {
        // The store's semantics: one place per (id, folder), the latest.
        let mut m = self.rows.lock().unwrap();
        for (id, l) in rows {
            let v = m.entry(id.clone()).or_default();
            v.retain(|o| o.folder != l.folder);
            v.push(l.clone());
        }
        Ok(())
    }
    fn forget_message_location(
        &self,
        _: squelch_core::types::AccountId,
        msg_id: &str,
        folder: &str,
    ) -> squelch_core::Result<()> {
        if let Some(v) = self.rows.lock().unwrap().get_mut(msg_id) {
            v.retain(|o| o.folder != folder);
        }
        Ok(())
    }
    fn content_digest(
        &self,
        _: squelch_core::types::AccountId,
        msg_id: &str,
    ) -> squelch_core::Result<Option<String>> {
        Ok(self.digests.lock().unwrap().get(msg_id).cloned())
    }
    fn record_content_digest(
        &self,
        _: squelch_core::types::AccountId,
        msg_id: &str,
        digest: &str,
    ) -> squelch_core::Result<()> {
        self.digests
            .lock()
            .unwrap()
            .entry(msg_id.into())
            .or_insert_with(|| digest.into());
        Ok(())
    }
    fn body_print(
        &self,
        _: squelch_core::types::AccountId,
        msg_id: &str,
    ) -> squelch_core::Result<Option<BodyPrint>> {
        Ok(self.prints.lock().unwrap().get(msg_id).cloned())
    }
    fn record_body_print(
        &self,
        _: squelch_core::types::AccountId,
        msg_id: &str,
        print: &BodyPrint,
    ) -> squelch_core::Result<()> {
        // First writer wins, as in the store.
        self.prints
            .lock()
            .unwrap()
            .entry(msg_id.into())
            .or_insert_with(|| print.clone());
        Ok(())
    }
}

#[test]
fn a_slot_without_smtp_is_the_run_auth_again_refusal() {
    let credential = ImapCredential {
        config: ImapAccountConfig {
            server: ImapServer {
                host: "127.0.0.1".into(),
                port: 1,
                tls: ImapTls::Plain,
                username: "u@x".into(),
            },
            ca_pem: None,
            smtp: None,
        },
        password: ImapPassword::new("pw"),
        smtp_password: None,
    };
    let source = Arc::new(
        ImapSource::from_config(
            1,
            &credential.config,
            credential.password.clone(),
            Arc::new(MemLocations::default()),
            Arc::new(Default::default()),
        )
        .unwrap(),
    );
    match ImapWriter::new(source, &credential, "u@x") {
        Err(WriteError::MissingCredential(m)) => assert_eq!(m, IMAP_NO_SMTP),
        Err(e) => panic!("{e}"),
        Ok(_) => panic!("built a writer with no SMTP"),
    }
    assert_eq!(
        IMAP_NO_SMTP,
        "run `squelchd auth --imap` again to enable sending"
    );
}

#[tokio::test]
async fn an_oversized_message_is_refused_before_any_server() {
    let credential = dev_credential("big@dev.test", ImapTls::Plain, 1);
    let writer = dev_writer(
        &credential,
        "big@dev.test",
        Arc::new(MemLocations::default()),
    );
    let raw = vec![b'a'; MAX_SEND_RAW_BYTES + 1];
    assert!(matches!(
        writer.send(&raw, None).await,
        Err(WriteError::Invalid(_))
    ));
}

/// SMTP SUCCEEDED, SENT DID NOT (contract §7): the send is still a
/// success, with a warning, and the server got the message once, with the
/// Bcc recipient in the envelope and no Bcc header in the bytes. IMAP here is
/// a port nothing listens on.
#[tokio::test]
async fn a_send_whose_sent_copy_fails_is_still_sent_with_a_warning() {
    use crate::write::smtp::tests::{AfterDot, scripted};
    use std::sync::atomic::Ordering;
    let (port, seen) = scripted(AfterDot::Accept).await;
    let mut credential = dev_credential("me@x.test", ImapTls::Plain, port);
    credential.config.server.host = "127.0.0.1".into();
    credential.config.server.port = 1;
    credential.config.server.tls = ImapTls::Plain;
    credential.config.smtp.as_mut().unwrap().host = "127.0.0.1".into();
    credential.smtp_password = Some(ImapPassword::new("good-pw"));
    let writer = dev_writer(&credential, "me@x.test", Arc::new(MemLocations::default()));
    let raw = b"To: a@x.test\r\nBcc: Secret <s@y.test>,\r\n t@y.test\r\nSubject: s\r\n\r\nbody\r\n";
    let sent = writer.send(raw, None).await.unwrap();
    assert!(sent.id.as_deref().is_some_and(|i| i.starts_with("imap:")));
    assert!(sent.warning.is_some());
    assert_eq!(seen.datas.load(Ordering::SeqCst), 1);
    let rcpts = seen.rcpts.lock().unwrap().join(" ");
    for r in ["<a@x.test>", "<s@y.test>", "<t@y.test>"] {
        assert!(rcpts.contains(r), "{rcpts}");
    }
    let msg = seen.messages.lock().unwrap()[0].clone();
    assert!(!msg.to_ascii_lowercase().contains("bcc"), "{msg}");
    assert!(
        !msg.contains("s@y.test") && !msg.contains("t@y.test"),
        "{msg}"
    );
    assert!(msg.starts_with("From: me@x.test\r\nDate: "), "{msg}");
    assert!(msg.contains("Message-ID: <"), "{msg}");
}

// ---- live: the Dovecot + Postfix test bed (scripts/imap-dev.sh) ----------------

const DEV_PASSWORD: &str = "squelch-dev-password";

fn dev_ca_pem() -> String {
    std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../scripts/imap-dev/.state/ca.crt"
    ))
    .unwrap_or_default()
}

fn dev_credential(user: &str, smtp_tls: ImapTls, smtp_port: u16) -> ImapCredential {
    ImapCredential {
        config: ImapAccountConfig {
            server: ImapServer {
                host: "localhost".into(),
                port: 10993,
                tls: ImapTls::Implicit,
                username: user.into(),
            },
            ca_pem: Some(dev_ca_pem()).filter(|s| !s.is_empty()),
            smtp: Some(SmtpServer {
                host: "localhost".into(),
                port: smtp_port,
                tls: smtp_tls,
            }),
        },
        password: ImapPassword::new(DEV_PASSWORD),
        smtp_password: None,
    }
}

fn dev_source(credential: &ImapCredential, locs: Arc<MemLocations>) -> Arc<ImapSource> {
    Arc::new(
        ImapSource::from_config(
            1,
            &credential.config,
            credential.password.clone(),
            locs,
            Arc::new(Default::default()),
        )
        .unwrap(),
    )
}

fn dev_writer(credential: &ImapCredential, from: &str, locs: Arc<MemLocations>) -> ImapWriter {
    ImapWriter::new(dev_source(credential, locs), credential, from)
        .unwrap()
        .with_look_schedule(vec![Duration::ZERO, Duration::from_millis(300)])
}

/// A fresh user per run: Dovecot's static passdb takes any name, so each
/// test reads mailboxes no earlier run has touched.
fn fresh_user(tag: &str) -> String {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{tag}-{n}@dev.test")
}

/// Every copy in `folder` of `user`'s mailbox whose Message-ID is `mid`
/// (bare), as (flags, raw bytes), waiting up to 15 s for `want` copies.
async fn copies(user: &str, folder: &str, mid: &str, want: usize) -> Vec<(Vec<String>, Vec<u8>)> {
    let credential = dev_credential(user, ImapTls::StartTls, 10587);
    let source = dev_source(&credential, Arc::new(MemLocations::default()));
    let mut last = Vec::new();
    for _ in 0..30 {
        let (mut s, _) = source.open_writer_session().await.unwrap();
        s.examine(folder).await.unwrap();
        let hits = s
            .uid_search(format!("HEADER Message-ID \"{mid}\""))
            .await
            .unwrap();
        last.clear();
        if !hits.is_empty() {
            let set = hits
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(",");
            let fetches: Vec<_> = s
                .uid_fetch(set, "(FLAGS BODY.PEEK[])")
                .await
                .unwrap()
                .try_collect()
                .await
                .unwrap();
            for f in fetches {
                let flags = f.flags().map(|fl| format!("{fl:?}")).collect();
                last.push((flags, f.body().unwrap_or_default().to_vec()));
            }
        }
        let _ = s.logout().await;
        if last.len() >= want {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    last
}

fn has_header(raw: &[u8], name: &str) -> bool {
    let text = String::from_utf8_lossy(raw);
    let head = text.split("\r\n\r\n").next().unwrap_or("");
    head.lines().any(|l| {
        l.split(':')
            .next()
            .is_some_and(|n| n.trim().eq_ignore_ascii_case(name))
    })
}

/// A Bcc header that reached the bed. Postfix's cleanup drops Bcc on its
/// own, which would hide a leak, so the bed's header check renames any Bcc it
/// receives to this first (scripts/imap-dev/smtp/bed_header_checks).
const BED_SAW_BCC: &str = "X-Imap-Dev-Saw-Bcc";

/// THE ROUND TRIP (contract "done means"): a send to self over STARTTLS
/// arrives in INBOX, Sent holds exactly one copy (appended by us, `\Seen`,
/// Bcc kept), the Bcc recipient gets it, and no Bcc header ever reached the
/// server.
#[tokio::test]
#[ignore = "needs scripts/imap-dev.sh start"]
async fn live_a_send_to_self_round_trips_with_one_sent_copy_and_a_blind_bcc() {
    let me = fresh_user("send");
    let blind = fresh_user("blind");
    let credential = dev_credential(&me, ImapTls::StartTls, 10587);
    let locs = Arc::new(MemLocations::default());
    let writer = dev_writer(&credential, &me, locs.clone());
    let raw = format!(
        "To: {me}\r\nBcc: Hidden <{blind}>\r\nSubject: phase3 round trip\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nhello over smtp\r\n"
    );
    let sent = writer.send(raw.as_bytes(), Some("ignored")).await.unwrap();
    assert!(sent.warning.is_none(), "{:?}", sent.warning);
    assert!(sent.thread_id.is_none());
    let id = sent.id.clone().unwrap();
    assert!(id.starts_with("imap:"));

    // The id is the stable id of the Message-ID the writer minted, and its
    // Sent location was recorded under it.
    let rows = locs.rows.lock().unwrap().get(&id).cloned().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].folder, "Sent");
    let mid = rows[0].message_id.clone().unwrap();
    assert_eq!(stable_id_for_message_id(&mid), id);

    let inbox = copies(&me, "INBOX", &mid, 1).await;
    assert_eq!(inbox.len(), 1, "one delivery to self");
    assert!(
        !has_header(&inbox[0].1, "Bcc"),
        "delivered copy carries Bcc"
    );
    assert!(
        !has_header(&inbox[0].1, BED_SAW_BCC),
        "a Bcc header was transmitted"
    );

    let sent_copies = copies(&me, "Sent", &mid, 1).await;
    assert_eq!(sent_copies.len(), 1, "exactly one copy in Sent");
    assert!(
        sent_copies[0].0.iter().any(|f| f.contains("Seen")),
        "{:?}",
        sent_copies[0].0
    );
    assert!(
        has_header(&sent_copies[0].1, "Bcc"),
        "the saved copy keeps Bcc"
    );
    assert!(has_header(&sent_copies[0].1, "From"));

    let bcc = copies(&blind, "INBOX", &mid, 1).await;
    assert_eq!(bcc.len(), 1, "the Bcc recipient got it");
    assert!(!has_header(&bcc[0].1, "Bcc"));
    assert!(!has_header(&bcc[0].1, BED_SAW_BCC));
}

/// Implicit TLS (465) logs in and sends just the same.
#[tokio::test]
#[ignore = "needs scripts/imap-dev.sh start"]
async fn live_implicit_tls_submission_works() {
    let me = fresh_user("implicit");
    let credential = dev_credential(&me, ImapTls::Implicit, 10465);
    let ep = SmtpEndpoint::from_config(&credential.config, &credential.password)
        .unwrap()
        .unwrap();
    smtp::check_login(&ep, SmtpTimeouts::default())
        .await
        .unwrap();
    let writer = dev_writer(&credential, &me, Arc::new(MemLocations::default()));
    let sent = writer
        .send(
            format!("To: {me}\r\nSubject: s\r\n\r\nb\r\n").as_bytes(),
            None,
        )
        .await
        .unwrap();
    assert!(sent.id.is_some());
}

/// A wrong password is one fixed line, with no password in it, and sends
/// nothing.
#[tokio::test]
#[ignore = "needs scripts/imap-dev.sh start"]
async fn live_a_refused_smtp_login_says_one_fixed_line() {
    let me = fresh_user("badpw");
    let mut credential = dev_credential(&me, ImapTls::StartTls, 10587);
    credential.smtp_password = Some(ImapPassword::new("wrong-password-xyzzy"));
    let ep = SmtpEndpoint::from_config(&credential.config, credential.smtp_login_password())
        .unwrap()
        .unwrap();
    let err = smtp::check_login(&ep, SmtpTimeouts::default())
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), smtp::SMTP_LOGIN_REFUSED);
    let writer = dev_writer(&credential, &me, Arc::new(MemLocations::default()));
    let err = writer
        .send(
            format!("To: {me}\r\nSubject: s\r\n\r\nb\r\n").as_bytes(),
            None,
        )
        .await
        .unwrap_err();
    assert!(!format!("{err} {err:?}").contains("xyzzy"));
    assert!(matches!(err, WriteError::Api { status: 401, .. }));
}

/// Amendment A2: looks at 0, 0.25, 0.5, 1.0 and 1.5 s, and no later.
#[test]
fn the_sent_looks_end_by_one_and_a_half_seconds() {
    let mut at = Duration::ZERO;
    let mut times = Vec::new();
    for w in LOOK_SCHEDULE {
        at += w;
        times.push(at.as_millis());
    }
    assert_eq!(times, vec![0, 250, 500, 1000, 1500]);
}

// ---- live: folder moves and flags (phase 3 step 2) ------------------------------

/// One message of the test mailbox as Dovecot itself reports it (`doveadm
/// fetch`), never through the code under test.
#[derive(Debug, Clone)]
struct Dove {
    uid: u32,
    flags: String,
    copy: String,
}

fn dove(user: &str, folder: &str) -> Vec<Dove> {
    let out = std::process::Command::new("docker")
        .args([
            "exec",
            "squelch-imap-dev",
            "/dovecot/bin/doveadm",
            "fetch",
            "-u",
            user,
            "uid flags hdr.x-test-copy",
            "mailbox",
            folder,
        ])
        .output()
        .expect("docker exec doveadm");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let mut rows: Vec<Dove> = Vec::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        let v = v.trim().to_string();
        match k.trim() {
            "uid" => rows.push(Dove {
                uid: v.parse().unwrap(),
                flags: String::new(),
                copy: String::new(),
            }),
            "flags" => rows.last_mut().unwrap().flags = v,
            "hdr.x-test-copy" => rows.last_mut().unwrap().copy = v,
            _ => {}
        }
    }
    rows
}

fn copies_tagged(user: &str, folder: &str, tag: &str) -> Vec<Dove> {
    dove(user, folder)
        .into_iter()
        .filter(|d| d.copy == tag)
        .collect()
}

/// A message whose identity headers are fixed and whose body and
/// `X-Test-Copy` (a header outside the identity digest) are `tag`'s.
fn test_message(me: &str, mid: &str, tag: &str) -> Vec<u8> {
    format!(
        "From: Someone <someone@dev.test>\r\nTo: {me}\r\nDate: Tue, 1 Sep 2026 10:00:00 +0000\r\n\
         Subject: step2 subject\r\nMessage-ID: <{mid}>\r\nX-Test-Copy: {tag}\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\r\nthe {tag} body\r\n"
    )
    .into_bytes()
}

/// APPEND `raw` to `folder` with `flags` (as `(\Seen)`), and where it landed:
/// a fresh user's folder has one writer, so UIDNEXT before is its UID.
async fn place(
    source: &ImapSource,
    folder: &str,
    raw: &[u8],
    flags: Option<&str>,
) -> MessageLocation {
    let (mut s, _) = source.open_writer_session().await.unwrap();
    let st = s.status(folder, "(UIDNEXT UIDVALIDITY)").await.unwrap();
    s.append(folder, flags, None, raw).await.unwrap();
    let _ = s.logout().await;
    let mid = smtp::first_header(raw, "Message-ID").and_then(|v| normalize_message_id(&v));
    MessageLocation {
        folder: folder.into(),
        uidvalidity: st.uid_validity.unwrap(),
        uid: st.uid_next.unwrap(),
        message_id: mid,
    }
}

/// A live writer on a fresh user, the id of a message with Message-ID
/// `mid`, and the user's address.
fn live_writer(tag: &str) -> (ImapWriter, Arc<MemLocations>, String) {
    let me = fresh_user(tag);
    let credential = dev_credential(&me, ImapTls::StartTls, 10587);
    let locs = Arc::new(MemLocations::default());
    let writer = dev_writer(&credential, &me, locs.clone());
    (writer, locs, me)
}

/// Put the original copy of `mid` into each of `folders` and record them
/// under its id, the way a sync would have; bind the id's print from them.
async fn original_in(writer: &ImapWriter, me: &str, mid: &str, folders: &[&str]) -> String {
    let id = stable_id_for_message_id(mid);
    let raw = test_message(me, mid, "original");
    for f in folders {
        let loc = place(&writer.source, f, &raw, None).await;
        writer.source.record_own_copy(&id, &raw, loc).unwrap();
    }
    let verified = writer.source.verified_locations(&id).await.unwrap();
    assert_eq!(verified.len(), folders.len(), "{verified:?}");
    id
}

/// An unrelated message already flagged `\Deleted` in `folder`, as another
/// client leaves one before it expunges: no write may remove it.
async fn deleted_bystander(writer: &ImapWriter, me: &str, folder: &str) {
    let raw = test_message(me, &format!("bystander-{folder}@dev.test"), "bystander");
    place(&writer.source, folder, &raw, Some("(\\Deleted)")).await;
}

fn assert_bystanders(me: &str, folders: &[&str]) {
    for f in folders {
        let b = copies_tagged(me, f, "bystander");
        assert_eq!(b.len(), 1, "the \\Deleted bystander in {f} was expunged");
        assert!(b[0].flags.contains("\\Deleted"), "{:?}", b[0]);
    }
}

fn rows_of(locs: &MemLocations, id: &str) -> Vec<MessageLocation> {
    locs.rows
        .lock()
        .unwrap()
        .get(id)
        .cloned()
        .unwrap_or_default()
}

/// Archive (the bed has \Archive), trash and not-spam, by MOVE and by COPY +
/// UID EXPUNGE, each verified with doveadm, the id's place recorded from
/// COPYUID, and every pre-existing `\Deleted` message still there.
async fn moves_round_trip(without_move: bool) {
    let (mut writer, locs, me) = live_writer(if without_move { "copymv" } else { "move" });
    if without_move {
        writer = writer.without_move();
    }
    let folders = ["INBOX", "Archive", "Junk", "Trash"];
    for f in folders {
        deleted_bystander(&writer, &me, f).await;
    }

    // Archive: INBOX -> Archive.
    let a = original_in(&writer, &me, "arch@dev.test", &["INBOX"]).await;
    writer.archive(&a).await.unwrap();
    assert!(copies_tagged(&me, "INBOX", "original").is_empty());
    let in_archive = copies_tagged(&me, "Archive", "original");
    assert_eq!(in_archive.len(), 1);
    let rows = rows_of(&locs, &a);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(
        (rows[0].folder.as_str(), rows[0].uid),
        ("Archive", in_archive[0].uid)
    );
    // Archiving again is nothing to do.
    writer.archive(&a).await.unwrap();
    assert_eq!(copies_tagged(&me, "Archive", "original").len(), 1);
    // +INBOX brings it back from the archive.
    writer.modify(&a, &["INBOX".into()], &[]).await.unwrap();
    assert_eq!(copies_tagged(&me, "INBOX", "original").len(), 1);
    assert!(copies_tagged(&me, "Archive", "original").is_empty());

    // +SPAM then not spam: INBOX -> Junk -> INBOX.
    writer
        .modify(&a, &["SPAM".into()], &["INBOX".into()])
        .await
        .unwrap();
    assert_eq!(copies_tagged(&me, "Junk", "original").len(), 1);
    assert!(copies_tagged(&me, "INBOX", "original").is_empty());
    writer.not_spam(&a).await.unwrap();
    assert_eq!(copies_tagged(&me, "INBOX", "original").len(), 1);
    assert!(copies_tagged(&me, "Junk", "original").is_empty());

    // Trash: INBOX -> Trash, never a delete.
    writer.trash(&a).await.unwrap();
    assert!(copies_tagged(&me, "INBOX", "original").is_empty());
    let trashed = copies_tagged(&me, "Trash", "original");
    assert_eq!(trashed.len(), 1);
    assert!(!trashed[0].flags.contains("\\Deleted"), "{:?}", trashed[0]);

    assert_bystanders(&me, &folders);
}

#[tokio::test]
#[ignore = "needs scripts/imap-dev.sh start"]
async fn live_moves_by_move_keep_every_deleted_bystander() {
    moves_round_trip(false).await;
}

#[tokio::test]
#[ignore = "needs scripts/imap-dev.sh start"]
async fn live_moves_by_copy_and_uid_expunge_keep_every_deleted_bystander() {
    moves_round_trip(true).await;
}

/// READ, UNREAD AND STAR on BOTH copies of a self-sent message (INBOX and
/// Sent), as Dovecot reports the flags; a bad label in the same request
/// changes nothing.
#[tokio::test]
#[ignore = "needs scripts/imap-dev.sh start"]
async fn live_flags_land_on_the_inbox_and_sent_copies() {
    let (writer, _, me) = live_writer("flags");
    deleted_bystander(&writer, &me, "INBOX").await;
    deleted_bystander(&writer, &me, "Sent").await;
    let id = original_in(&writer, &me, "self@dev.test", &["INBOX", "Sent"]).await;
    let flags = |f: &str| copies_tagged(&me, f, "original")[0].flags.clone();

    writer.modify(&id, &[], &["UNREAD".into()]).await.unwrap();
    writer.modify(&id, &["STARRED".into()], &[]).await.unwrap();
    for f in ["INBOX", "Sent"] {
        assert!(flags(f).contains("\\Seen"), "{f}: {}", flags(f));
        assert!(flags(f).contains("\\Flagged"), "{f}: {}", flags(f));
    }
    writer
        .modify(&id, &["UNREAD".into()], &["STARRED".into()])
        .await
        .unwrap();
    for f in ["INBOX", "Sent"] {
        assert!(!flags(f).contains("\\Seen"), "{f}: {}", flags(f));
        assert!(!flags(f).contains("\\Flagged"), "{f}: {}", flags(f));
    }
    // Validated whole: STARRED is not applied next to a label IMAP lacks.
    let err = writer
        .modify(&id, &["STARRED".into(), "IMPORTANT".into()], &[])
        .await
        .unwrap_err();
    assert!(
        matches!(&err, WriteError::Invalid(m) if m.contains("IMPORTANT")),
        "{err}"
    );
    for f in ["INBOX", "Sent"] {
        assert!(!flags(f).contains("\\Flagged"), "{f}: {}", flags(f));
    }
    // Trash takes both copies.
    writer.trash(&id).await.unwrap();
    assert_eq!(copies_tagged(&me, "Trash", "original").len(), 2);
    assert!(copies_tagged(&me, "INBOX", "original").is_empty());
    assert!(copies_tagged(&me, "Sent", "original").is_empty());
    assert_bystanders(&me, &["INBOX", "Sent"]);
}

/// A LOOKALIKE (phase 2 F1: the same Message-ID and identity headers over
/// another body) recorded under the id is never the copy acted on: archive
/// leaves it in INBOX, a star goes on the original only, trash takes the
/// original only, and the reads serve the original's bytes.
#[tokio::test]
#[ignore = "needs scripts/imap-dev.sh start"]
async fn live_a_lookalike_is_never_the_copy_acted_on() {
    let (writer, locs, me) = live_writer("lookalike");
    let mid = "twin@dev.test";
    let id = original_in(&writer, &me, mid, &["Junk"]).await;
    // The lookalike lands in INBOX and a sighting records it under the id.
    let fake = test_message(&me, mid, "lookalike");
    let at = place(&writer.source, "INBOX", &fake, None).await;
    locs.record_message_locations(1, &[(id.clone(), at)])
        .unwrap();
    assert_eq!(rows_of(&locs, &id).len(), 2);

    let verified = writer.source.verified_locations(&id).await.unwrap();
    assert_eq!(verified.len(), 1, "{verified:?}");
    assert_eq!(verified[0].folder, "Junk");

    let raw =
        squelch_core::mail::gmail::decode_raw_b64url(&writer.fetch_raw(&id).await.unwrap().raw_b64)
            .unwrap();
    assert!(String::from_utf8_lossy(&raw).contains("the original body"));

    // Archive takes the INBOX copy, and the id has none that is its own.
    writer.archive(&id).await.unwrap();
    assert_eq!(copies_tagged(&me, "INBOX", "lookalike").len(), 1);
    assert!(copies_tagged(&me, "Archive", "lookalike").is_empty());

    writer.modify(&id, &["STARRED".into()], &[]).await.unwrap();
    assert!(
        copies_tagged(&me, "Junk", "original")[0]
            .flags
            .contains("\\Flagged")
    );
    assert!(
        !copies_tagged(&me, "INBOX", "lookalike")[0]
            .flags
            .contains("\\Flagged")
    );

    writer.trash(&id).await.unwrap();
    assert_eq!(copies_tagged(&me, "Trash", "original").len(), 1);
    assert!(copies_tagged(&me, "Trash", "lookalike").is_empty());
    assert_eq!(copies_tagged(&me, "INBOX", "lookalike").len(), 1);
}

/// The lookups: parent headers from the verified copy, an exact Message-ID
/// in Sent (a Message-ID CONTAINING it is not a hit), and a miss is `None`.
#[tokio::test]
#[ignore = "needs scripts/imap-dev.sh start"]
async fn live_lookups_read_the_verified_copy_and_search_exactly() {
    let (writer, _, me) = live_writer("lookups");
    let id = original_in(&writer, &me, "parent@dev.test", &["Sent"]).await;
    let h = writer.parent_headers(&id).await.unwrap();
    assert_eq!(h.message_id.as_deref(), Some("<parent@dev.test>"));
    assert_eq!(h.from.as_deref(), Some("Someone <someone@dev.test>"));
    assert_eq!(h.to.as_deref(), Some(me.as_str()));

    assert_eq!(
        writer
            .find_by_rfc822_msgid("<parent@dev.test>")
            .await
            .unwrap(),
        Some(id)
    );
    let longer = test_message(&me, "xparent@dev.test.example", "other");
    place(&writer.source, "Sent", &longer, None).await;
    assert_eq!(
        writer
            .find_by_rfc822_msgid("<arent@dev.test>")
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        writer
            .find_by_rfc822_msgid("<nowhere@dev.test>")
            .await
            .unwrap(),
        None
    );
    assert!(matches!(
        writer
            .modify("imap:unknown", &["STARRED".into()], &[])
            .await,
        Err(WriteError::Api { status: 404, .. })
    ));
}

// ---- amendment A3: a maybe-sent scheduled send is never sent again ------------

/// An [`OutboxLedger`] in memory, which can be told to fail.
#[derive(Default)]
struct MemOutbox {
    marks: Mutex<HashMap<String, OutboxMark>>,
    broken: std::sync::atomic::AtomicBool,
}

impl MemOutbox {
    fn mark(&self, token: &str) -> Option<OutboxMark> {
        self.marks.lock().unwrap().get(token).cloned()
    }
    fn check(&self) -> Result<(), String> {
        if self.broken.load(std::sync::atomic::Ordering::SeqCst) {
            Err("ledger down".into())
        } else {
            Ok(())
        }
    }
}

impl OutboxLedger for MemOutbox {
    fn get(&self, token: &str) -> Result<Option<OutboxMark>, String> {
        self.check()?;
        Ok(self.mark(token))
    }
    fn set(&self, token: &str, mark: &OutboxMark) -> Result<(), String> {
        self.check()?;
        self.marks
            .lock()
            .unwrap()
            .insert(token.into(), mark.clone());
        Ok(())
    }
    fn clear(&self, token: &str) -> Result<(), String> {
        self.check()?;
        self.marks.lock().unwrap().remove(token);
        Ok(())
    }
}

#[test]
fn an_outbox_mark_round_trips_and_garbage_is_doubt() {
    for m in [
        OutboxMark::Submitting {
            message_id: "a@b".into(),
        },
        OutboxMark::Accepted {
            message_id: "a@b".into(),
        },
    ] {
        assert_eq!(OutboxMark::decode(&m.encode()), m);
    }
    for garbage in ["", "accepted", "accepted ", "weird x@y"] {
        assert!(
            matches!(OutboxMark::decode(garbage), OutboxMark::Submitting { .. }),
            "{garbage:?}"
        );
    }
}

fn is_doubt(r: &Result<Option<String>, WriteError>) -> bool {
    matches!(r, Err(WriteError::Transport(m)) if m == MAY_HAVE_BEEN_SENT)
}

/// THE DECISION: only "no hit AND no marker" is "not sent". A hit wins
/// (and clears the marker); `Accepted` is sent; `Submitting`, an unreadable
/// ledger and no ledger at all are doubt.
#[test]
fn only_no_hit_and_no_marker_is_not_sent() {
    let ledger = MemOutbox::default();
    let mid = "tok@passband.app";
    let sub = OutboxMark::Submitting {
        message_id: mid.into(),
    };
    assert_eq!(
        outbox_answer(None, mid, "tok", Some(&ledger)).unwrap(),
        None
    );

    ledger.set("tok", &sub).unwrap();
    assert!(is_doubt(&outbox_answer(None, mid, "tok", Some(&ledger))));

    ledger
        .set(
            "tok",
            &OutboxMark::Accepted {
                message_id: mid.into(),
            },
        )
        .unwrap();
    assert_eq!(
        outbox_answer(None, mid, "tok", Some(&ledger)).unwrap(),
        Some(stable_id_for_message_id(mid))
    );

    ledger.set("tok", &sub).unwrap();
    let mut hit = loc(3);
    hit.message_id = Some(mid.into());
    assert_eq!(
        outbox_answer(Some(hit), mid, "tok", Some(&ledger)).unwrap(),
        Some(stable_id_for_message_id(mid))
    );
    assert_eq!(ledger.mark("tok"), None, "a hit clears the marker");

    assert!(is_doubt(&outbox_answer(None, mid, "tok", None)));
    ledger
        .broken
        .store(true, std::sync::atomic::Ordering::SeqCst);
    assert!(is_doubt(&outbox_answer(None, mid, "tok", Some(&ledger))));
}

/// A send whose marker cannot be written is refused with nothing said to
/// SMTP (the port answers nothing) and NOT maybe-sent material: no marker
/// was left.
#[tokio::test]
async fn a_scheduled_send_whose_marker_cannot_be_written_is_not_sent() {
    let credential = dev_credential("nomark@dev.test", ImapTls::Plain, 1);
    let ledger = Arc::new(MemOutbox::default());
    ledger
        .broken
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let writer =
        dev_writer(&credential, "nomark@dev.test", Default::default()).with_outbox(ledger.clone());
    let raw = format!(
        "To: x@dev.test\r\nMessage-ID: <{0}@passband.app>\r\n{OUTBOX_HEADER}: {0}\r\nSubject: s\r\n\r\nb\r\n",
        "ab".repeat(24)
    );
    match writer.send(raw.as_bytes(), None).await {
        Err(WriteError::Api { status: 503, .. }) => {}
        other => panic!("{other:?}"),
    }
}

/// A fired scheduled send as the worker builds it: the minted Message-ID
/// and the outbox token beside it.
fn scheduled_raw(to: &str, token: &str) -> String {
    format!(
        "To: {to}\r\nSubject: scheduled\r\nMessage-ID: <{token}@passband.app>\r\n\
         {OUTBOX_HEADER}: {token}\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nlater\r\n"
    )
}

fn fresh_token() -> String {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{n:048x}")
}

const WINDOW: fn() -> (chrono::DateTime<chrono::Utc>, chrono::DateTime<chrono::Utc>) =
    || (chrono::Utc::now(), chrono::Utc::now());

/// The ordinary path: sent, one copy in Sent, the marker gone, and the
/// recovery question answered by the copy.
#[tokio::test]
#[ignore = "needs scripts/imap-dev.sh start"]
async fn live_a_fired_scheduled_send_is_found_and_leaves_no_marker() {
    let (writer, _, me) = live_writer("sched-ok");
    let ledger = Arc::new(MemOutbox::default());
    let writer = writer.with_outbox(ledger.clone());
    let token = fresh_token();
    let sent = writer
        .send(scheduled_raw(&me, &token).as_bytes(), None)
        .await
        .unwrap();
    let mid = format!("{token}@passband.app");
    assert_eq!(sent.id, Some(stable_id_for_message_id(&mid)));
    assert_eq!(ledger.mark(&token), None, "Sent has it: no marker kept");
    assert_eq!(copies(&me, "Sent", &mid, 1).await.len(), 1);
    let found = writer
        .find_scheduled_send(&format!("<{mid}>"), WINDOW(), &|| false)
        .await
        .unwrap();
    assert_eq!(found, sent.id);

    // Never sent at all: no copy, no marker, the one "not sent".
    let other = fresh_token();
    assert_eq!(
        writer
            .find_scheduled_send(&format!("<{other}@passband.app>"), WINDOW(), &|| false)
            .await
            .unwrap(),
        None
    );
}

/// THE CRASH BETWEEN SMTP AND THE APPEND: the mail went (it is in the
/// INBOX) and Sent has no copy, the case Gmail's "not in Sent = not sent"
/// would send twice. The marker answers: `Accepted` is the id, never a
/// resend; one that never got past `Submitting` (a crash before SMTP
/// answered) is doubt, an `Err`, which the worker never fires on.
#[tokio::test]
#[ignore = "needs scripts/imap-dev.sh start"]
async fn live_a_crash_between_smtp_and_sent_is_never_sent_again() {
    let (writer, _, me) = live_writer("sched-crash");
    let ledger = Arc::new(MemOutbox::default());
    let writer = writer.with_outbox(ledger.clone()).crash_after_smtp();
    let token = fresh_token();
    let mid = format!("{token}@passband.app");
    assert!(
        writer
            .send(scheduled_raw(&me, &token).as_bytes(), None)
            .await
            .is_err()
    );
    assert_eq!(copies(&me, "INBOX", &mid, 1).await.len(), 1, "it went");
    assert!(
        copies(&me, "Sent", &mid, 0).await.is_empty(),
        "no Sent copy"
    );
    assert!(matches!(
        ledger.mark(&token),
        Some(OutboxMark::Accepted { .. })
    ));
    let asked = format!("<{mid}>");
    assert_eq!(
        writer
            .find_scheduled_send(&asked, WINDOW(), &|| false)
            .await
            .unwrap(),
        Some(stable_id_for_message_id(&mid))
    );

    // The crash a moment earlier: SMTP took it, the marker never moved on.
    ledger
        .set(
            &token,
            &OutboxMark::Submitting {
                message_id: mid.clone(),
            },
        )
        .unwrap();
    let r = writer
        .find_scheduled_send(&asked, WINDOW(), &|| false)
        .await;
    assert!(is_doubt(&r), "{r:?}");
}

/// The token alone finds it, exactly: a copy in Sent whose Message-ID a
/// server rewrote still answers, by its own Message-ID's id; a token that
/// merely CONTAINS the asked one does not.
#[tokio::test]
#[ignore = "needs scripts/imap-dev.sh start"]
async fn live_the_outbox_token_finds_a_rewritten_copy_exactly() {
    let (writer, _, me) = live_writer("sched-token");
    let writer = writer.with_outbox(Arc::new(MemOutbox::default()));
    let token = fresh_token();
    let rewritten = format!(
        "To: {me}\r\nSubject: s\r\nMessage-ID: <rewritten-{token}@server.example>\r\n\
         {OUTBOX_HEADER}: {token}\r\n\r\nb\r\n"
    );
    let longer = format!(
        "To: {me}\r\nSubject: s\r\nMessage-ID: <other-{token}@server.example>\r\n\
         {OUTBOX_HEADER}: x{token}\r\n\r\nb\r\n"
    );
    place(&writer.source, "Sent", longer.as_bytes(), None).await;
    let asked = format!("<{token}@passband.app>");
    assert_eq!(
        writer
            .find_scheduled_send(&asked, WINDOW(), &|| false)
            .await
            .unwrap(),
        None
    );
    place(&writer.source, "Sent", rewritten.as_bytes(), None).await;
    assert_eq!(
        writer
            .find_scheduled_send(&asked, WINDOW(), &|| false)
            .await
            .unwrap(),
        Some(stable_id_for_message_id(&format!(
            "rewritten-{token}@server.example"
        )))
    );
}
