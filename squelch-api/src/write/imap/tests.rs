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
        let mut m = self.rows.lock().unwrap();
        for (id, l) in rows {
            m.entry(id.clone()).or_default().push(l.clone());
        }
        Ok(())
    }
    fn forget_message_location(
        &self,
        _: squelch_core::types::AccountId,
        _: &str,
        _: &str,
    ) -> squelch_core::Result<()> {
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
            .insert(msg_id.into(), digest.into());
        Ok(())
    }
    fn body_print(
        &self,
        _: squelch_core::types::AccountId,
        _: &str,
    ) -> squelch_core::Result<Option<BodyPrint>> {
        Ok(None)
    }
    fn record_body_print(
        &self,
        _: squelch_core::types::AccountId,
        _: &str,
        _: &BodyPrint,
    ) -> squelch_core::Result<()> {
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
