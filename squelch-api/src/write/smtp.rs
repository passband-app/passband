//! SMTP SUBMISSION for an IMAP account (issue #575 phase 3, contract §7, §9):
//! the transport half of [`super::imap::ImapWriter::send`], and the message
//! preparation Gmail's API used to do for us.
//!
//! THE POLICY is IMAP's, to the byte where it can be:
//! - the socket and TLS are ours, made under squelch-core's
//!   [`tls_client_config`] (web PKI plus the account's sealed CA, verification
//!   always on), then handed to lettre already secured. lettre parses replies,
//!   encodes AUTH and dot-stuffs DATA, nothing more;
//! - `Implicit` (465) is TLS from the first byte. `StartTls` (587) speaks
//!   EHLO and STARTTLS one byte at a time over the bare socket, so nothing a
//!   network attacker appends after the `220` is ever read as if it came
//!   over TLS; a server that does not offer STARTTLS is refused. `Plain` is
//!   for loopback only, with the connected peer address checked;
//! - AUTH only ever goes over TLS (or to loopback), and a refused AUTH is
//!   one fixed line ([`SMTP_LOGIN_REFUSED`]): no server text, no password;
//! - every step has a timeout, and the message itself gets
//!   [`super::send_budget`], the Gmail path's.
//!
//! NEVER RETRY AFTER DATA. Once the `DATA` command has been accepted, a
//! failure that is not the server's own negative reply (a timeout, a reset,
//! a reply we could not read) is [`SmtpError::MaybeSent`]: the message may be
//! on its way, and sending it again is how a user gets duplicates. Only a
//! failure before that point, or the server's explicit refusal of the
//! message, is [`SmtpError::NotSent`].

use std::net::Ipv4Addr;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use lettre::Address;
use lettre::transport::smtp::authentication::{Credentials, Mechanism};
use lettre::transport::smtp::client::AsyncSmtpConnection;
use lettre::transport::smtp::commands::{Data, Mail, Rcpt};
use lettre::transport::smtp::extension::{ClientId, Extension, MailBodyParameter, MailParameter};
use squelch_core::mail::imap::{
    ImapAccountConfig, ImapPassword, ImapTls, SmtpServer, plaintext_allowed, tls_client_config,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::rustls::pki_types::ServerName;

/// What a refused SMTP login says, whatever the server said: its text can
/// echo the username, and nothing in it helps more than this line.
pub const SMTP_LOGIN_REFUSED: &str = "smtp login refused: check the username and app password";

/// Where and as whom an account submits. `Debug` redacts the password
/// ([`ImapPassword`]).
#[derive(Debug, Clone)]
pub struct SmtpEndpoint {
    pub server: SmtpServer,
    pub username: String,
    pub password: ImapPassword,
    /// The TLS policy's input: the IMAP config whose `ca_pem` both protocols
    /// trust.
    pub tls: std::sync::Arc<tokio_rustls::rustls::ClientConfig>,
}

impl SmtpEndpoint {
    /// The endpoint an account's sealed config names, or `None` when it has
    /// no SMTP config (a slot from before sending existed).
    pub fn from_config(
        config: &ImapAccountConfig,
        password: &ImapPassword,
    ) -> Result<Option<Self>, SmtpError> {
        let Some(server) = config.smtp.clone() else {
            return Ok(None);
        };
        let tls = tls_client_config(config).map_err(|_| {
            SmtpError::NotSent("smtp: the stored CA certificate is unusable".into())
        })?;
        Ok(Some(Self {
            server,
            username: config.server.username.clone(),
            password: password.clone(),
            tls,
        }))
    }
}

/// Bounds on each step.
#[derive(Debug, Clone, Copy)]
pub struct SmtpTimeouts {
    /// TCP connect, then separately the TLS handshake.
    pub connect: Duration,
    /// Any one command and its reply (greeting, EHLO, AUTH, MAIL, RCPT,
    /// DATA, QUIT).
    pub command: Duration,
    /// The longest the server may say nothing (and take nothing) while
    /// something waits on it: IMAP's stall bound. Not a total, so a large
    /// message over a slow uplink keeps going.
    pub stall: Duration,
}

impl Default for SmtpTimeouts {
    fn default() -> Self {
        Self {
            connect: Duration::from_secs(30),
            command: Duration::from_secs(60),
            stall: Duration::from_secs(120),
        }
    }
}

/// How a submission failed. Every message is ours and carries at most an
/// SMTP reply code: never server text, never a credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SmtpError {
    /// Nothing was sent, and the server said so for good: a permanent (5yz)
    /// refusal of the sender, a recipient, `DATA` or the message itself.
    /// Sending it again as it is will be refused again.
    Refused(String),
    /// Nothing was sent: a failure before `DATA` was accepted (no connection,
    /// TLS, a timeout, a transient 4yz), or a transient refusal of the
    /// message itself. Safe to retry.
    NotSent(String),
    /// The login was refused ([`SMTP_LOGIN_REFUSED`]).
    Auth,
    /// THE MESSAGE MAY HAVE BEEN SENT: the connection failed after `DATA`.
    /// Never retried.
    MaybeSent(String),
}

impl std::fmt::Display for SmtpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SmtpError::Refused(m) | SmtpError::NotSent(m) => write!(f, "{m}"),
            SmtpError::Auth => f.write_str(SMTP_LOGIN_REFUSED),
            SmtpError::MaybeSent(m) => write!(
                f,
                "the message may have been sent ({m}); it was not retried, check Sent before \
                 sending it again"
            ),
        }
    }
}

fn not_sent(what: &str) -> impl FnOnce(lettre::transport::smtp::Error) -> SmtpError + '_ {
    move |e| match e.status() {
        Some(code) if e.is_permanent() => {
            SmtpError::Refused(format!("smtp server refused {what} ({code})"))
        }
        Some(code) => SmtpError::NotSent(format!("smtp server refused {what} for now ({code})")),
        None if e.is_timeout() => SmtpError::NotSent(format!("smtp {what}: timed out")),
        None => SmtpError::NotSent(format!("smtp {what}: connection failed")),
    }
}

async fn bounded<T>(
    limit: Duration,
    what: &str,
    fut: impl std::future::Future<Output = Result<T, SmtpError>>,
) -> Result<T, SmtpError> {
    tokio::time::timeout(limit, fut)
        .await
        .map_err(|_| SmtpError::NotSent(format!("smtp {what}: timed out")))?
}

/// The EHLO name: an address literal, as RFC 5321 §4.1.4 allows a client
/// with no stable name to send. It says nothing about the machine.
fn hello() -> ClientId {
    ClientId::Ipv4(Ipv4Addr::LOCALHOST)
}

fn host_of(server: &SmtpServer) -> &str {
    server.host.trim_start_matches('[').trim_end_matches(']')
}

/// THE SOCKET LETTRE TALKS OVER: the stream, with
/// - a greeting we made up served first, when there is one: after STARTTLS
///   the server sends none, and lettre's connection starts by reading one.
///   The real conversation (EHLO onward) is all TLS;
/// - a STALL TIMER, IMAP's ([`squelch_core::mail::imap::Transport`]): while a
///   read or a write waits, the server must make progress within `stall` or
///   the operation fails with `TimedOut`. Progress is any byte read or any
///   write accepted, so a large message over a slow link is never cut off;
///   only silence is.
#[derive(Debug)]
struct Wire<S> {
    greeting: &'static [u8],
    inner: S,
    stall: Duration,
    last_progress: tokio::time::Instant,
    timer: Option<Pin<Box<tokio::time::Sleep>>>,
}

impl<S> Wire<S> {
    fn new(inner: S, greeting: &'static [u8], stall: Duration) -> Self {
        Self {
            greeting,
            inner,
            stall,
            last_progress: tokio::time::Instant::now(),
            timer: None,
        }
    }

    fn progressed(&mut self) {
        self.last_progress = tokio::time::Instant::now();
        self.timer = None;
    }

    /// The inner poll came back `Pending`: has the server been silent too
    /// long? Arms the timer so this task is woken when it would be.
    fn check_stall(&mut self, cx: &mut Context<'_>) -> Poll<std::io::Error> {
        let due = self.last_progress + self.stall;
        let timer = self
            .timer
            .get_or_insert_with(|| Box::pin(tokio::time::sleep_until(due)));
        if timer.deadline() != due {
            timer.as_mut().reset(due);
        }
        match timer.as_mut().poll(cx) {
            Poll::Ready(()) => Poll::Ready(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "the smtp server stopped answering",
            )),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for Wire<S> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        if !this.greeting.is_empty() {
            let n = this.greeting.len().min(buf.remaining());
            buf.put_slice(&this.greeting[..n]);
            this.greeting = &this.greeting[n..];
            return Poll::Ready(Ok(()));
        }
        match Pin::new(&mut this.inner).poll_read(cx, buf) {
            Poll::Pending => this.check_stall(cx).map(Err),
            ready => {
                this.progressed();
                ready
            }
        }
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Wire<S> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let this = self.get_mut();
        match Pin::new(&mut this.inner).poll_write(cx, buf) {
            Poll::Pending => this.check_stall(cx).map(Err),
            ready => {
                this.progressed();
                ready
            }
        }
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        match Pin::new(&mut this.inner).poll_flush(cx) {
            Poll::Pending => this.check_stall(cx).map(Err),
            ready => {
                this.progressed();
                ready
            }
        }
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

impl lettre::transport::smtp::client::AsyncTokioStream
    for Wire<tokio_rustls::client::TlsStream<TcpStream>>
{
    fn peer_addr(&self) -> std::io::Result<std::net::SocketAddr> {
        self.inner.get_ref().0.peer_addr()
    }
}

impl lettre::transport::smtp::client::AsyncTokioStream for Wire<TcpStream> {
    fn peer_addr(&self) -> std::io::Result<std::net::SocketAddr> {
        self.inner.peer_addr()
    }
}

/// TCP keepalive on the socket, as IMAP's has: a peer that vanished (a NAT
/// that dropped the mapping, a laptop that slept) is noticed by the kernel.
/// Best effort: a platform that refuses it still has the stall timer.
fn set_keepalive(tcp: &TcpStream) {
    let ka = socket2::TcpKeepalive::new()
        .with_time(Duration::from_secs(60))
        .with_interval(Duration::from_secs(15));
    let _ = socket2::SockRef::from(tcp).set_tcp_keepalive(&ka);
}

/// The longest a QUIT on a connection being given up on may take. lettre's
/// `abort` sends QUIT and waits for the reply with no bound of its own, and
/// it runs exactly when the server is not answering (security review F1).
const ABORT_BOUND: Duration = Duration::from_secs(5);

/// Give up on `conn`: QUIT within [`ABORT_BOUND`] (or the command bound, if
/// shorter), and the socket closed either way, since dropping it closes it.
async fn abandon(mut conn: AsyncSmtpConnection, t: SmtpTimeouts) {
    let _ = tokio::time::timeout(t.command.min(ABORT_BOUND), conn.abort()).await;
}

/// One reply off the bare socket, read a byte at a time so nothing past its
/// last line is consumed. Its code, and the text of every line.
async fn read_reply_unbuffered(tcp: &mut TcpStream) -> Result<(u16, Vec<String>), SmtpError> {
    let broken = || SmtpError::NotSent("smtp: unreadable reply before STARTTLS".into());
    let mut lines = Vec::new();
    loop {
        let mut line = Vec::new();
        loop {
            let b = tcp.read_u8().await.map_err(|_| broken())?;
            line.push(b);
            if line.ends_with(b"\r\n") {
                break;
            }
            if line.len() > 1000 {
                return Err(broken());
            }
        }
        let text = String::from_utf8_lossy(&line[..line.len() - 2]).into_owned();
        if text.len() < 3 || !text.is_char_boundary(3) {
            return Err(broken());
        }
        let code: u16 = text[..3].parse().map_err(|_| broken())?;
        let more = text.as_bytes().get(3) == Some(&b'-');
        lines.push(text.get(4..).unwrap_or("").to_string());
        if !more {
            if lines.len() > 100 {
                return Err(broken());
            }
            return Ok((code, lines));
        }
        if lines.len() > 100 {
            return Err(broken());
        }
    }
}

/// Connect, secure and greet: an EHLO'd connection, TLS unless the host is
/// loopback with `Plain`. Not yet logged in.
async fn open(ep: &SmtpEndpoint, t: SmtpTimeouts) -> Result<AsyncSmtpConnection, SmtpError> {
    let host = host_of(&ep.server);
    if ep.server.tls == ImapTls::Plain && !plaintext_allowed(host) {
        return Err(SmtpError::NotSent(format!(
            "smtp: refusing a plaintext connection to {host}: only loopback may skip TLS"
        )));
    }
    let tcp = tokio::time::timeout(t.connect, TcpStream::connect((host, ep.server.port)))
        .await
        .map_err(|_| SmtpError::NotSent("smtp tcp connect: timed out".into()))?
        .map_err(|_| SmtpError::NotSent("smtp tcp connect: failed".into()))?;
    set_keepalive(&tcp);
    let name = || {
        ServerName::try_from(host.to_string())
            .map_err(|_| SmtpError::NotSent("smtp: the host is not a valid TLS name".into()))
    };
    let handshake = |tcp: TcpStream| {
        let name = name();
        async move {
            tokio::time::timeout(
                t.connect,
                TlsConnector::from(ep.tls.clone()).connect(name?, tcp),
            )
            .await
            .map_err(|_| SmtpError::NotSent("smtp tls handshake: timed out".into()))?
            .map_err(|_| SmtpError::NotSent("smtp tls handshake: failed".into()))
        }
    };
    let connected = match ep.server.tls {
        ImapTls::Plain => {
            // The name said loopback; the socket must end there too.
            let peer = tcp
                .peer_addr()
                .map_err(|_| SmtpError::NotSent("smtp: no peer address".into()))?;
            if !peer.ip().is_loopback() {
                return Err(SmtpError::NotSent(
                    "smtp: plaintext host resolved off this machine; refusing".into(),
                ));
            }
            AsyncSmtpConnection::connect_with_transport(
                Box::new(Wire::new(tcp, b"", t.stall)),
                &hello(),
            )
            .await
        }
        ImapTls::Implicit => {
            let tls = handshake(tcp).await?;
            AsyncSmtpConnection::connect_with_transport(
                Box::new(Wire::new(tls, b"", t.stall)),
                &hello(),
            )
            .await
        }
        ImapTls::StartTls => {
            let mut tcp = tcp;
            bounded(t.command, "starttls", async {
                let (code, _) = read_reply_unbuffered(&mut tcp).await?;
                if code != 220 {
                    return Err(SmtpError::NotSent(format!(
                        "smtp server refused the connection ({code})"
                    )));
                }
                tcp.write_all(format!("EHLO {}\r\n", hello()).as_bytes())
                    .await
                    .map_err(|_| SmtpError::NotSent("smtp ehlo: connection failed".into()))?;
                let (code, lines) = read_reply_unbuffered(&mut tcp).await?;
                if code != 250 {
                    return Err(SmtpError::NotSent(format!(
                        "smtp server refused EHLO ({code})"
                    )));
                }
                if !lines
                    .iter()
                    .any(|l| l.split_whitespace().next() == Some("STARTTLS"))
                {
                    return Err(SmtpError::NotSent(
                        "smtp server does not offer STARTTLS; refusing to log in in the clear"
                            .into(),
                    ));
                }
                tcp.write_all(b"STARTTLS\r\n")
                    .await
                    .map_err(|_| SmtpError::NotSent("smtp starttls: connection failed".into()))?;
                let (code, _) = read_reply_unbuffered(&mut tcp).await?;
                if code != 220 {
                    return Err(SmtpError::NotSent(format!(
                        "smtp server refused STARTTLS ({code})"
                    )));
                }
                Ok(())
            })
            .await?;
            let tls = handshake(tcp).await?;
            AsyncSmtpConnection::connect_with_transport(
                Box::new(Wire::new(tls, b"220 tls\r\n", t.stall)),
                &hello(),
            )
            .await
        }
    };
    connected.map_err(not_sent("EHLO"))
}

/// What a login the server put off says: a 4yz to AUTH is the server's
/// "not now" (RFC 4954's 454), not a wrong password.
pub const SMTP_LOGIN_LATER: &str = "the mail server couldn't sign in right now; try again";

/// How long until `deadline`, capped at `step`: each step's own bound, never
/// past the send's.
fn within(step: Duration, deadline: Option<tokio::time::Instant>) -> Duration {
    match deadline {
        Some(d) => step.min(d.saturating_duration_since(tokio::time::Instant::now())),
        None => step,
    }
}

/// [`bounded`] by [`within`], and refused outright once `deadline` has
/// passed: a timer at zero still lets a ready step through.
async fn bounded_until<T>(
    step: Duration,
    deadline: Option<tokio::time::Instant>,
    what: &str,
    fut: impl std::future::Future<Output = Result<T, SmtpError>>,
) -> Result<T, SmtpError> {
    let limit = within(step, deadline);
    if limit.is_zero() {
        return Err(SmtpError::NotSent(format!("smtp {what}: out of time")));
    }
    bounded(limit, what, fut).await
}

/// Connect, secure, greet and log in. The connection-level steps share one
/// bound per step; the whole thing is bounded too, and by `deadline`.
async fn open_and_login(
    ep: &SmtpEndpoint,
    t: SmtpTimeouts,
    deadline: Option<tokio::time::Instant>,
) -> Result<AsyncSmtpConnection, SmtpError> {
    let mut conn = bounded_until(
        t.connect * 2 + t.command * 2,
        deadline,
        "connect",
        open(ep, t),
    )
    .await?;
    let mechanisms = [Mechanism::Plain, Mechanism::Login];
    if !mechanisms.iter().any(|m| {
        conn.server_info()
            .supports_feature(Extension::Authentication(*m))
    }) {
        abandon(conn, t).await;
        return Err(SmtpError::NotSent(
            "smtp server offers no AUTH PLAIN or LOGIN".into(),
        ));
    }
    let creds = Credentials::new(ep.username.clone(), ep.password.expose_secret().to_string());
    match tokio::time::timeout(within(t.command, deadline), conn.auth(&mechanisms, &creds)).await {
        Ok(Ok(_)) => Ok(conn),
        Ok(Err(e)) => {
            abandon(conn, t).await;
            Err(match e.status() {
                // A wrong username or password, or a mechanism refused for
                // good: the fixed line.
                Some(_) if e.is_permanent() => SmtpError::Auth,
                // 454 and friends: the server could not check it right now.
                Some(_) => SmtpError::NotSent(SMTP_LOGIN_LATER.into()),
                None if e.is_timeout() => SmtpError::NotSent("smtp auth: timed out".into()),
                None => SmtpError::NotSent("smtp auth: connection failed".into()),
            })
        }
        Err(_) => {
            abandon(conn, t).await;
            Err(SmtpError::NotSent("smtp auth: timed out".into()))
        }
    }
}

/// THE SETUP CHECK `squelchd auth --imap` runs before it stores anything:
/// EHLO, STARTTLS (or implicit TLS), AUTH, QUIT. No mail is sent.
pub async fn check_login(ep: &SmtpEndpoint, t: SmtpTimeouts) -> Result<(), SmtpError> {
    let mut conn = open_and_login(ep, t, None).await?;
    let _ = tokio::time::timeout(t.command, conn.quit()).await;
    Ok(())
}

/// SUBMIT one message: `from` as MAIL FROM, every `rcpts` as RCPT TO, then
/// `bytes` (already stripped of Bcc, see [`transmitted_copy`]) as DATA,
/// within `budget`. A recipient the server refuses fails the whole send
/// before DATA, so no subset of the audience ever gets it.
pub async fn submit(
    ep: &SmtpEndpoint,
    t: SmtpTimeouts,
    from: &str,
    rcpts: &[String],
    bytes: &[u8],
    budget: Duration,
) -> Result<(), SmtpError> {
    submit_until(ep, t, from, rcpts, bytes, budget, None).await
}

/// [`submit`], all of it over by `deadline` when there is one: a step that
/// would run past it before `DATA` is [`SmtpError::NotSent`] (nothing went),
/// and the message's own budget is cut to it, which past `DATA` is
/// [`SmtpError::MaybeSent`] as any other cut there is.
pub async fn submit_until(
    ep: &SmtpEndpoint,
    t: SmtpTimeouts,
    from: &str,
    rcpts: &[String],
    bytes: &[u8],
    budget: Duration,
    deadline: Option<tokio::time::Instant>,
) -> Result<(), SmtpError> {
    let parse = |a: &str| {
        a.parse::<Address>()
            .map_err(|_| SmtpError::NotSent(format!("not a deliverable address: {a}")))
    };
    let from = parse(from)?;
    let rcpts: Vec<Address> = rcpts.iter().map(|r| parse(r)).collect::<Result<_, _>>()?;
    if rcpts.is_empty() {
        return Err(SmtpError::NotSent("the message has no recipient".into()));
    }
    let mut conn = open_and_login(ep, t, deadline).await?;
    let mut params = Vec::new();
    let ascii = |a: &Address| AsRef::<str>::as_ref(a).is_ascii();
    let non_ascii_addr = !ascii(&from) || rcpts.iter().any(|r| !ascii(r));
    if non_ascii_addr {
        if !conn.server_info().supports_feature(Extension::SmtpUtfEight) {
            abandon(conn, t).await;
            return Err(SmtpError::NotSent(
                "an address is not ASCII and the server does not take SMTPUTF8".into(),
            ));
        }
        params.push(MailParameter::SmtpUtfEight);
    }
    if !bytes.is_ascii() {
        if !conn.server_info().supports_feature(Extension::EightBitMime) {
            abandon(conn, t).await;
            return Err(SmtpError::NotSent(
                "the message is not 7-bit and the server does not take 8BITMIME".into(),
            ));
        }
        params.push(MailParameter::Body(MailBodyParameter::EightBitMime));
    }
    let before_data = async {
        bounded_until(t.command, deadline, "MAIL FROM", async {
            conn.command(Mail::new(Some(from), params))
                .await
                .map_err(not_sent("MAIL FROM"))
        })
        .await?;
        for r in rcpts {
            bounded_until(t.command, deadline, "RCPT TO", async {
                conn.command(Rcpt::new(r, vec![]))
                    .await
                    .map_err(not_sent("a recipient"))
            })
            .await?;
        }
        bounded_until(t.command, deadline, "DATA", async {
            conn.command(Data).await.map_err(not_sent("DATA"))
        })
        .await?;
        Ok::<(), SmtpError>(())
    }
    .await;
    if let Err(e) = before_data {
        abandon(conn, t).await;
        return Err(e);
    }
    // lettre ends DATA with `CRLF . CRLF` whatever the message ends with, so
    // a message's own final line break is left off here: that CRLF is it.
    // Otherwise every delivered copy gains a blank line the copy saved to
    // Sent lacks, and the two print as different messages.
    let bytes = bytes.strip_suffix(b"\r\n").unwrap_or(bytes);
    // PAST THIS LINE THE MESSAGE MAY BE ON ITS WAY.
    let sent = match tokio::time::timeout(within(budget, deadline), conn.message(bytes)).await {
        Err(_) => Err(SmtpError::MaybeSent(
            "the server did not answer in time".into(),
        )),
        Ok(Ok(_)) => Ok(()),
        // The server's own negative reply to the final dot: refused, so not
        // sent (RFC 5321 §4.2.5).
        Ok(Err(e)) if e.status().is_some() => Err(not_sent("the message")(e)),
        Ok(Err(_)) => Err(SmtpError::MaybeSent(
            "the connection failed after the message was handed over".into(),
        )),
    };
    if sent.is_ok() {
        let _ = tokio::time::timeout(t.command.min(ABORT_BOUND), conn.quit()).await;
    } else {
        abandon(conn, t).await;
    }
    sent
}

// ---- the message itself ------------------------------------------------------

/// Where the header block ends: the index of the blank line's first byte,
/// and the index of the body's first byte. A message with no blank line is
/// all header.
fn split_header(raw: &[u8]) -> (usize, usize) {
    let mut i = 0;
    while i < raw.len() {
        let end = raw[i..]
            .iter()
            .position(|&b| b == b'\n')
            .map_or(raw.len(), |p| i + p + 1);
        let line = &raw[i..end];
        if line == b"\r\n" || line == b"\n" {
            return (i, end);
        }
        i = end;
    }
    (raw.len(), raw.len())
}

/// The header fields of a block, each with its continuation lines, as
/// (name, bytes) with the bytes exactly as written.
fn header_fields(block: &[u8]) -> Vec<(&[u8], &[u8])> {
    let mut out: Vec<(&[u8], &[u8])> = Vec::new();
    let mut i = 0;
    let mut start = 0;
    while i < block.len() {
        let end = block[i..]
            .iter()
            .position(|&b| b == b'\n')
            .map_or(block.len(), |p| i + p + 1);
        let continuation = matches!(block[i], b' ' | b'\t');
        if !continuation && i > start {
            let field = &block[start..i];
            out.push((field_name(field), field));
            start = i;
        }
        i = end;
    }
    if start < block.len() {
        let field = &block[start..];
        out.push((field_name(field), field));
    }
    out
}

fn field_name(field: &[u8]) -> &[u8] {
    let colon = field.iter().position(|&b| b == b':').unwrap_or(0);
    field[..colon].trim_ascii()
}

fn is_blind(name: &[u8]) -> bool {
    name.eq_ignore_ascii_case(b"bcc") || name.eq_ignore_ascii_case(b"resent-bcc")
}

/// THE BYTES THAT GO OVER SMTP: `raw` with every `Bcc` (and `Resent-Bcc`)
/// field removed, continuation lines and all, however many there are. Gmail's
/// API did this for us; an SMTP server relays what it is given. The body is
/// never touched, even if a line in it starts `Bcc:`.
pub fn transmitted_copy(raw: &[u8]) -> Vec<u8> {
    let (head_end, _) = split_header(raw);
    let mut out = Vec::with_capacity(raw.len());
    for (name, field) in header_fields(&raw[..head_end]) {
        if !is_blind(name) {
            out.extend_from_slice(field);
        }
    }
    out.extend_from_slice(&raw[head_end..]);
    out
}

/// THE ENVELOPE RECIPIENTS: every address in every `To`, `Cc` and `Bcc`
/// field of `raw` (groups included, display names dropped), lowercased for
/// de-duplication, in first-seen order.
pub fn envelope_recipients(raw: &[u8]) -> Vec<String> {
    use mail_parser::{HeaderName, HeaderValue, MessageParser};
    let (head_end, body_start) = split_header(raw);
    // Headers only: the body plays no part in who the message goes to.
    let mut head = raw[..body_start].to_vec();
    if body_start == head_end {
        head.extend_from_slice(b"\r\n");
    }
    let Some(msg) = MessageParser::default().parse_headers(&head) else {
        return Vec::new();
    };
    let mut out: Vec<String> = Vec::new();
    for h in msg.headers() {
        if !matches!(h.name, HeaderName::To | HeaderName::Cc | HeaderName::Bcc) {
            continue;
        }
        let HeaderValue::Address(addrs) = &h.value else {
            continue;
        };
        for a in addrs.iter() {
            let Some(addr) = a
                .address
                .as_deref()
                .map(str::trim)
                .filter(|s| s.contains('@'))
            else {
                continue;
            };
            let key = addr.to_ascii_lowercase();
            if !out.contains(&key) {
                out.push(key);
            }
        }
    }
    out
}

/// The value of the first `name` field in `raw`'s header block, unfolded.
pub fn first_header(raw: &[u8], name: &str) -> Option<String> {
    let (head_end, _) = split_header(raw);
    squelch_core::mail::imap::header_values(&raw[..head_end], name)
        .into_iter()
        .next()
}

/// THE COPY TO SAVE AND SEND, before Bcc comes off: `raw` with the headers
/// Gmail's API used to fill in added when the composer left them out, at the
/// top of the block: `From` (the account), `Date` (now) and `Message-ID`
/// (`minted`, needed so the look in Sent and the stable id can find it).
/// Headers already present are kept as they are.
///
/// IT ENDS IN CRLF, as every delivered copy does (`submit` sends that final
/// CRLF as the one `DATA`'s `CRLF . CRLF` begins with). The copy saved to
/// Sent must have the same body bytes, or the delivered copy and the saved
/// one print differently and the source takes one of them for a lookalike
/// (seen end to end: a door-composed body has no final line break).
pub fn with_submission_headers(
    raw: &[u8],
    from: &str,
    now: chrono::DateTime<chrono::Utc>,
    minted: &str,
) -> Vec<u8> {
    let mut head = String::new();
    if first_header(raw, "From").is_none() {
        head.push_str(&format!("From: {from}\r\n"));
    }
    if first_header(raw, "Date").is_none() {
        head.push_str(&format!("Date: {}\r\n", now.to_rfc2822()));
    }
    if first_header(raw, "Message-ID").is_none() {
        head.push_str(&format!("Message-ID: {minted}\r\n"));
    }
    let mut out = head.into_bytes();
    out.extend_from_slice(raw);
    let mut out = crlf_only(&out);
    if !out.ends_with(b"\r\n") {
        out.extend_from_slice(b"\r\n");
    }
    out
}

/// EVERY LINE BREAK A CRLF (security review F2): a lone CR and a lone LF
/// each become one. RFC 5321 §2.3.8 forbids a bare CR in DATA, and a server
/// that reads one as a line end turns `X<CR>.<CR><LF>` in a body into the end
/// of the message, and what follows into commands under the user's login.
/// As CRLF that is a line holding one `.`, which DATA's dot-stuffing escapes.
/// Done to the copy that is both saved and transmitted, so they stay the
/// same bytes.
pub fn crlf_only(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len() + raw.len() / 64);
    let mut i = 0;
    while i < raw.len() {
        match raw[i] {
            b'\r' if raw.get(i + 1) == Some(&b'\n') => {
                out.extend_from_slice(b"\r\n");
                i += 2;
                continue;
            }
            b'\r' | b'\n' => out.extend_from_slice(b"\r\n"),
            b => out.push(b),
        }
        i += 1;
    }
    out
}

/// The longest line RFC 5322 §2.1.1 allows, CRLF not counted.
const MAX_LINE: usize = 998;

/// SAFE FOR ANY RELAY (correctness review F4): a text part written with no
/// `Content-Transfer-Encoding` (or `7bit`/`8bit`) whose body has a line over
/// [`MAX_LINE`] or any non-ASCII byte is re-encoded quoted-printable, its
/// header saying so. An MTA is entitled to fold an over-long line (Postfix
/// splits past 2048) and to down-convert 8-bit, and either makes the
/// delivered copy's body differ from the one saved to Sent, which then
/// prints as another message. Quoted-printable has neither problem. The
/// whole message, then each part of a multipart, one level of nesting at a
/// time; anything else (an attachment, a part already encoded) is left
/// exactly as it is. `raw` is CRLF-only ([`crlf_only`]).
pub fn transfer_safe(raw: &[u8]) -> Vec<u8> {
    reencode_entity(raw, 0).unwrap_or_else(|| raw.to_vec())
}

/// How deep [`transfer_safe`] follows multiparts (mixed > related >
/// alternative is three).
const MAX_MIME_DEPTH: usize = 4;

/// `entity` re-encoded, or `None` when nothing in it needed to be.
fn reencode_entity(entity: &[u8], depth: usize) -> Option<Vec<u8>> {
    let (head_end, body_start) = split_header(entity);
    if body_start == head_end {
        return None;
    }
    let head = &entity[..head_end];
    let body = &entity[body_start..];
    let value = |name: &str| {
        squelch_core::mail::imap::header_values(head, name)
            .into_iter()
            .next()
            .map(|v| v.trim().to_ascii_lowercase())
    };
    let ctype = value("Content-Type").unwrap_or_default();
    let plain_cte = match value("Content-Transfer-Encoding").as_deref() {
        None | Some("7bit") | Some("8bit") => true,
        Some(_) => false,
    };
    if !plain_cte {
        return None;
    }
    if ctype.starts_with("multipart/") {
        if depth >= MAX_MIME_DEPTH {
            return None;
        }
        let boundary = boundary_of(
            &squelch_core::mail::imap::header_values(head, "Content-Type")
                .into_iter()
                .next()
                .unwrap_or_default(),
        )?;
        let body = reencode_multipart(body, boundary.as_bytes(), depth)?;
        let mut out = entity[..body_start].to_vec();
        out.extend_from_slice(&body);
        return Some(out);
    }
    if !(ctype.is_empty() || ctype.starts_with("text/")) || !needs_qp(body) {
        return None;
    }
    let mut out = Vec::with_capacity(entity.len() + entity.len() / 4);
    for (name, field) in header_fields(head) {
        if !name.eq_ignore_ascii_case(b"content-transfer-encoding") {
            out.extend_from_slice(field);
        }
    }
    out.extend_from_slice(b"Content-Transfer-Encoding: quoted-printable\r\n");
    out.extend_from_slice(&entity[head_end..body_start]);
    out.extend_from_slice(super::mime::qp_encode_bytes(body).as_bytes());
    Some(out)
}

/// Does `body` have a line too long for SMTP, or a byte that is not 7-bit?
fn needs_qp(body: &[u8]) -> bool {
    !body.is_ascii() || body.split(|&b| b == b'\n').any(|l| l.len() > MAX_LINE + 1)
}

/// The `boundary` parameter of a Content-Type value, quoted or not.
fn boundary_of(ctype: &str) -> Option<String> {
    let lower = ctype.to_ascii_lowercase();
    let at = lower.find("boundary=")? + "boundary=".len();
    let rest = &ctype[at..];
    let b = match rest.strip_prefix('"') {
        Some(q) => q.split('"').next()?,
        None => rest.split(|c: char| c == ';' || c.is_whitespace()).next()?,
    };
    (!b.is_empty()).then(|| b.to_string())
}

/// A multipart body with each of its parts through [`reencode_entity`], or
/// `None` when none changed. The delimiters, the preamble and the epilogue
/// are kept byte for byte; a part is what lies between one delimiter line's
/// CRLF and the CRLF that starts the next delimiter (RFC 2046 §5.1.1).
fn reencode_multipart(body: &[u8], boundary: &[u8], depth: usize) -> Option<Vec<u8>> {
    let mut dash = b"--".to_vec();
    dash.extend_from_slice(boundary);
    // Every delimiter line: (its start, the index after its CRLF, closing?).
    let mut delims: Vec<(usize, usize, bool)> = Vec::new();
    let mut i = 0;
    while i < body.len() {
        let end = body[i..]
            .iter()
            .position(|&b| b == b'\n')
            .map_or(body.len(), |p| i + p + 1);
        let line = &body[i..end];
        if let Some(rest) = line.strip_prefix(dash.as_slice()) {
            let closing = rest.starts_with(b"--");
            let tail = if closing { &rest[2..] } else { rest };
            if tail
                .iter()
                .all(|b| matches!(b, b' ' | b'\t' | b'\r' | b'\n'))
            {
                delims.push((i, end, closing));
                if closing {
                    break;
                }
            }
        }
        i = end;
    }
    if delims.len() < 2 {
        return None;
    }
    let mut out = body[..delims[0].1].to_vec();
    let mut changed = false;
    for w in delims.windows(2) {
        let (_, part_start, _) = w[0];
        let (next_start, next_end, _) = w[1];
        // The CRLF before the next delimiter belongs to the delimiter.
        let part_end = next_start.saturating_sub(2).max(part_start);
        let part = &body[part_start..part_end];
        match reencode_entity(part, depth + 1) {
            Some(p) => {
                changed = true;
                out.extend_from_slice(&p);
            }
            None => out.extend_from_slice(part),
        }
        out.extend_from_slice(&body[part_end..next_end]);
    }
    let last_end = delims.last().map_or(body.len(), |d| d.1);
    out.extend_from_slice(&body[last_end..]);
    changed.then_some(out)
}

/// A fresh `Message-ID` for a send the composer did not give one: 24 bytes
/// of OS entropy as hex at the ACCOUNT's domain (amendment A1), the right-hand
/// side a server and a recipient expect of the sender. A domain that is not
/// plain dotted letters, digits and hyphens (an address with no `@`, a
/// literal) falls back to the scheduled send's host.
pub fn mint_message_id(account: &str) -> Option<String> {
    use base64::Engine as _;
    let token = squelch_core::tracking::mint_token().ok()?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(token)
        .ok()?;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    Some(format!("<{hex}@{}>", message_id_domain(account)))
}

/// The right-hand side [`mint_message_id`] uses for `account`.
fn message_id_domain(account: &str) -> String {
    let domain = account.rsplit_once('@').map(|(_, d)| d).unwrap_or("");
    let plain = !domain.is_empty()
        && domain.len() <= 253
        && domain.contains('.')
        && domain.split('.').all(|label| {
            !label.is_empty()
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        });
    if plain {
        domain.to_ascii_lowercase()
    } else {
        "passband.app".to_string()
    }
}

#[cfg(test)]
pub(crate) mod tests;
