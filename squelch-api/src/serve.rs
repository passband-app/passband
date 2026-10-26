//! The accept loop behind every door the daemon binds: hyper's connection
//! builder with a LIVE TIMER, under the graceful shutdown `axum::serve` gave.
//!
//! `axum::serve` builds its hyper connections without a timer, and hyper's
//! header-read timeout, the one thing that closes a connection whose client
//! never finishes (or never starts) a request, is inert without one: a client
//! that opened a socket and sent half a request head held that socket, its
//! task and its buffers until the client went away, which a client that is
//! not really a client never does. This loop hands the builder a
//! [`TokioTimer`] and sets the timeout explicitly ([`HEADER_READ_TIMEOUT`]),
//! so a connection that has not produced a request head in that long is
//! closed, the memory and the descriptor with it.
//!
//! The same timer closes a KEEP-ALIVE connection that has sat idle between
//! requests for that long, which is what sets the value. Traefik fronts a
//! hosted tenant and keeps its upstream connections for 90 s by default; a
//! proxy that writes a request onto a connection the daemon closed a moment
//! earlier answers the client 502, and the way to never be in that race is
//! for the proxy to be the side that closes first. The daemon therefore
//! waits LONGER than the proxy: five minutes bounds a half-open connection
//! where nothing did, and leaves every idle keep-alive to the proxy's own
//! reaper. A client talking to a self-hosted daemon directly has the same
//! race with no proxy to lose it for it: a GET written onto a connection
//! closed in that instant is retried by `URLSession`, a POST is reported as
//! a lost connection. That takes five idle minutes first, which an app that
//! polls while open never gives it; five rather than two to make it rarer
//! still, at the price of a half-open socket's 20 KB for that long.
//!
//! HTTP/1 only. Nothing speaks HTTP/2 to this daemon (the Swift client and
//! a fronting Traefik both use HTTP/1 to it), and an h2c connection would
//! have no timer: hyper's HTTP/2 server has no header-read timeout, so a
//! peer that sent the 24-byte preface and nothing more would be held the
//! old way. `http1_only` on the builder closes that, which is also why the
//! loop uses `serve_connection` rather than the upgrades variant (where the
//! setting is ignored); no door here upgrades a connection.
//!
//! Every request carries its peer address as `ConnectInfo<SocketAddr>`, as
//! `into_make_service_with_connect_info` would have provided, so a handler
//! that checks the peer (the calendar consent listener) can be served here.

use std::future::Future;
use std::io;
use std::pin::pin;
use std::time::Duration;

use axum::extract::ConnectInfo;
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use hyper_util::server::conn::auto::Builder;
use hyper_util::server::graceful::GracefulShutdown;
use hyper_util::service::TowerToHyperService;
use tokio::net::TcpListener;
use tower::ServiceExt;

/// How long a connection may go without completing a request head before it
/// is closed: a half-open request, or a keep-alive connection with nothing
/// on it. LONGER than the 90 s a fronting Traefik keeps an idle upstream
/// connection, so the proxy closes first (module docs).
pub const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(300);

/// Serve `app` on `listener` until `shutdown` resolves, then stop accepting,
/// let every open connection finish the request it has in flight, and
/// return. A response body still streaming (`GET /client/events`) holds its
/// connection open: the state's shutdown watch is what ends those.
pub async fn serve(
    listener: TcpListener,
    app: axum::Router,
    shutdown: impl Future<Output = ()>,
) -> io::Result<()> {
    serve_with_header_timeout(listener, app, shutdown, HEADER_READ_TIMEOUT).await
}

/// [`serve`] with the header-read timeout chosen by the caller. The daemon
/// uses [`HEADER_READ_TIMEOUT`]; tests use something they can wait out.
pub async fn serve_with_header_timeout(
    listener: TcpListener,
    app: axum::Router,
    shutdown: impl Future<Output = ()>,
    header_read_timeout: Duration,
) -> io::Result<()> {
    let mut builder = Builder::new(TokioExecutor::new()).http1_only();
    builder
        .http1()
        .timer(TokioTimer::new())
        .header_read_timeout(header_read_timeout);
    let graceful = GracefulShutdown::new();
    let mut shutdown = pin!(shutdown);
    // Names the door in the one line this loop can log: the daemon runs
    // several of these.
    let bound = listener
        .local_addr()
        .map_or_else(|_| "?".to_string(), |a| a.to_string());

    loop {
        let (stream, peer) = tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok(accepted) => accepted,
                Err(e) => {
                    accept_failed(&bound, e).await;
                    continue;
                }
            },
            () = &mut shutdown => break,
        };
        // An `Arc` clone of the router per connection, wrapped (not
        // re-layered) to stamp the peer on each request. `axum::serve` built
        // the router afresh for every connection (`Router::with_state(())`
        // in its make-service), a copy of the whole route table held for the
        // connection's life.
        let service = app
            .clone()
            .map_request(move |mut req: axum::extract::Request<_>| {
                req.extensions_mut().insert(ConnectInfo(peer));
                req
            });
        let conn = builder
            .serve_connection(TokioIo::new(stream), TowerToHyperService::new(service))
            .into_owned();
        let conn = graceful.watch(conn);
        tokio::spawn(async move {
            // A connection that ended in error (a reset, a malformed head, the
            // timer above) is the client's problem; nothing of ours to log.
            let _ = conn.await;
        });
    }

    drop(listener);
    graceful.shutdown().await;
    Ok(())
}

/// What `axum::serve` does with an accept error: a connection-level failure
/// (the peer reset before we got to it) is nothing, anything else (out of
/// descriptors, say) is logged and waited out for a second so the loop does
/// not spin.
async fn accept_failed(bound: &str, e: io::Error) {
    if matches!(
        e.kind(),
        io::ErrorKind::ConnectionRefused
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::ConnectionReset
    ) {
        return;
    }
    eprintln!("squelch-api: accept error on {bound}: {e}");
    tokio::time::sleep(Duration::from_secs(1)).await;
}
