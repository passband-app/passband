//! What bounds a `/client/*` request: a ceiling on the time it may take to
//! PRODUCE ITS RESPONSE, chosen per route, and a cap on how many of the
//! door's store calls may be on blocking threads at once.
//!
//! Neither existed. A request whose store call never returned (a wedged
//! writer, a lock held by a long ingest) held its connection, and the
//! blocking thread its store call sat on, for as long as the client cared to
//! wait; and every such request took a blocking thread of its own, up to
//! tokio's 512, after which the next store call anywhere in the daemon queued
//! behind them. Now the client is answered 504 (code `timeout`) at the
//! ceiling, and no more than [`STORE_CALL_SLOTS`] store calls are on threads
//! at once, the rest waiting on the async side (under their own ceiling) for
//! a slot.
//!
//! The ceiling is on the response HEAD, not the body: a stream that keeps
//! going after its headers (`GET /client/events`) is untouched. A request
//! past its ceiling is dropped, which stops everything it was awaiting
//! EXCEPT a store call already running on a blocking thread, which finishes
//! on its own, holding its thread and its slot until the store answers; that
//! the slot goes with the thread is what bounds the total a wedge can
//! collect (see [`STORE_CALL_SLOTS`]).

use std::time::Duration;

use axum::extract::{MatchedPath, Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::error::ApiError;
use crate::state::ApiState;

/// The ceiling for a route that does local work: the store and a render.
/// Above the Swift client's own 15 s idle wait, so the daemon is never the
/// side that gives up first on a request the client is still waiting for; a
/// stuck one is cut 5 s after the client stopped listening. A route that
/// awaits a model is `Upstream`: its budget alone is under this, but the
/// store writes around it can each wait SQLite's 5 s busy handler.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// The ceiling for a route that makes ONE call out before it answers: to
/// Gmail or the control plane (an archive, a label, a reply's recipients, a
/// scheduled forward's fetch, a calendar write), each with its own timeout
/// of at most 60 s and a possible 30 s token refresh ahead of it; or to a
/// model (a rule's disposition, a suggested rule, the headline, the smart
/// block), each with an 8-12 s budget and store writes around it. This sits
/// above the sum, the two or three store writes around the call (each able
/// to wait SQLite's 5 s busy handler) included, so a stalled upstream comes
/// back as the handler's own 502 or 504, carrying its reason, rather than a
/// timeout that says nothing.
pub const UPSTREAM_REQUEST_TIMEOUT: Duration = Duration::from_secs(180);

/// The ceiling for the routes whose own budgets run to minutes: a send
/// ([`crate::write::send_budget`] grows with the message, to about 13
/// min for the largest attachments allowed, on top of the fetches around
/// it), an attachment upload (25 MiB at whatever the uplink gives), the
/// shredder pass, the invite minter's serial sends, and the assistant relay's
/// wait for one of its four slots. Each bounds its own network calls; this
/// is the backstop for a store call under them that never returns, and it
/// sits above the largest send's worst case (a test pins that) so it never
/// cuts a send Gmail may already have accepted.
pub const LONG_REQUEST_TIMEOUT: Duration = Duration::from_secs(20 * 60);

/// How many of the door's store calls may be on blocking threads at once
/// ([`crate::handlers::blocking`]). The cap is on the CALL, not a
/// concurrency layer over the routes, because a request is what a layer's
/// permit rides and a blocking thread is what has to be bounded: a request
/// cut at its ceiling hands such a permit back while its store call keeps
/// its thread until the store answers, and with a queue waiting at the door
/// every permit handed back admitted the next request onto a thread of its
/// own (measured: six hundred requests against a wedged store took the
/// daemon to tokio's 512 blocking threads with a route-level cap of 64 in
/// place, exactly as without one). The slot here is moved INTO the closure,
/// so it is held for as long as the thread is, and a request waiting for one
/// waits on the async side, where its ceiling cuts it without a thread
/// having been taken.
///
/// The store runs one call at a time behind its mutex, so this is not a
/// parallelism knob: anything above one buys nothing but parked threads. It
/// sits well above what the client runs in parallel (six per host on
/// HTTP/1.1), so no ordinary burst waits here, and far below 512.
///
/// Every store call the `/client/*` tree makes takes one: the handlers',
/// the event stream's replay pump, the client-action stamp the bearer check
/// leaves behind. The bearer check's own device-token lookup has its four
/// ([`crate::auth::DEVICE_AUTH_CONCURRENCY`]), held the same way
/// ([`crate::handlers::blocking_on`]), so an unauthenticated guess never
/// waits on a slot an authenticated request holds. The pair, console and
/// pixel routes outside this tree reach the store through these same
/// slots, each behind a permit count of its own and under no ceiling: a
/// wedge that fills the slots has them wait, holding their own permit,
/// rather than park a thread.
pub const STORE_CALL_SLOTS: usize = 64;

/// Which ceiling a route runs under. Anything not in [`ROUTE_CEILINGS`] is
/// `Local`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestCeiling {
    /// [`REQUEST_TIMEOUT`].
    Local,
    /// [`UPSTREAM_REQUEST_TIMEOUT`].
    Upstream,
    /// [`LONG_REQUEST_TIMEOUT`].
    Long,
}

/// Every (method, route template) that is not [`RequestCeiling::Local`]. A
/// handler that awaits the network before it answers owes this table a row;
/// a test walks it against the real router, so a renamed route is caught.
pub const ROUTE_CEILINGS: &[(&str, &str, RequestCeiling)] = &[
    ("POST", "/client/actions/send", RequestCeiling::Long),
    ("POST", "/client/compose/attachments", RequestCeiling::Long),
    ("POST", "/client/assistant/messages", RequestCeiling::Long),
    ("POST", "/client/invites", RequestCeiling::Long),
    ("POST", "/client/shredder/run", RequestCeiling::Long),
    // The routes that await a model (`ROUTE_CLASSES` in `http_metrics`
    // charts the same ones as model time).
    ("GET", "/client/rules/suggest", RequestCeiling::Upstream),
    ("POST", "/client/rules", RequestCeiling::Upstream),
    ("PUT", "/client/rules/{id}", RequestCeiling::Upstream),
    ("GET", "/client/v2/fye/headline", RequestCeiling::Upstream),
    (
        "GET",
        "/client/unsubscribes/smart_block",
        RequestCeiling::Upstream,
    ),
    ("POST", "/client/actions/archive", RequestCeiling::Upstream),
    ("POST", "/client/actions/not_spam", RequestCeiling::Upstream),
    ("POST", "/client/actions/label", RequestCeiling::Upstream),
    (
        "GET",
        "/client/messages/{message_id}/reply_recipients",
        RequestCeiling::Upstream,
    ),
    // A scheduled forward fetches the original from Gmail before it is
    // stored; a plain scheduled send is a store write.
    ("POST", "/client/scheduled", RequestCeiling::Upstream),
    ("POST", "/client/calendar/events", RequestCeiling::Upstream),
    (
        "PATCH",
        "/client/calendar/events/{calendar_id}/{id}",
        RequestCeiling::Upstream,
    ),
    (
        "DELETE",
        "/client/calendar/events/{calendar_id}/{id}",
        RequestCeiling::Upstream,
    ),
];

/// The ceiling a (method, template) runs under.
pub fn route_ceiling(method: &str, template: &str) -> RequestCeiling {
    route_lookup(ROUTE_CEILINGS, method, template, RequestCeiling::Local)
}

/// The lookup the door's route tables share (this one and
/// [`crate::http_metrics::ROUTE_CLASSES`]): the row for (method, template),
/// else `default`. A template under another verb is another row.
pub(crate) fn route_lookup<T: Copy>(
    table: &[(&str, &str, T)],
    method: &str,
    template: &str,
    default: T,
) -> T {
    table
        .iter()
        .find(|(m, t, _)| *m == method && *t == template)
        .map_or(default, |(_, _, value)| *value)
}

/// The three ceilings as a value, so a test can shrink them. The daemon runs
/// the constants.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RequestTimeouts {
    pub local: Duration,
    pub upstream: Duration,
    pub long: Duration,
}

impl Default for RequestTimeouts {
    fn default() -> Self {
        Self {
            local: REQUEST_TIMEOUT,
            upstream: UPSTREAM_REQUEST_TIMEOUT,
            long: LONG_REQUEST_TIMEOUT,
        }
    }
}

impl RequestTimeouts {
    /// Every ceiling the same `d`: the shape every test wants.
    pub fn all(d: Duration) -> Self {
        Self {
            local: d,
            upstream: d,
            long: d,
        }
    }

    pub fn for_ceiling(&self, ceiling: RequestCeiling) -> Duration {
        match ceiling {
            RequestCeiling::Local => self.local,
            RequestCeiling::Upstream => self.upstream,
            RequestCeiling::Long => self.long,
        }
    }
}

/// The ceiling, as middleware over the `/client/*` tree. Applied with
/// `Router::layer`, so it runs after routing and sees the matched template;
/// a request that matched nothing gets the local ceiling.
///
/// A request past its ceiling is answered 504, not 408: a 408 says the
/// request never arrived in full and invites the client to send it again,
/// and this request arrived, and may have done its work (a store write
/// already on its thread still lands). 504 is the status the calendar
/// writes already answer for a deadline passed with the outcome unknown
/// (with their own code, `uncertain`); this one carries code `timeout`,
/// and the Swift client reads any 5xx the same way.
pub(crate) async fn request_timeout(
    State(state): State<ApiState>,
    req: Request,
    next: Next,
) -> Response {
    let ceiling = req
        .extensions()
        .get::<MatchedPath>()
        .map_or(RequestCeiling::Local, |p| {
            route_ceiling(req.method().as_str(), p.as_str())
        });
    let allowed = state.request_timeouts.for_ceiling(ceiling);
    match tokio::time::timeout(allowed, next.run(req)).await {
        Ok(resp) => resp,
        Err(_elapsed) => ApiError::new(StatusCode::GATEWAY_TIMEOUT, "request timed out")
            .with_code("timeout")
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_off_the_table_run_under_the_local_ceiling() {
        assert_eq!(route_ceiling("GET", "/client/stats"), RequestCeiling::Local);
        assert_eq!(
            route_ceiling("POST", "/client/actions/send"),
            RequestCeiling::Long
        );
        // The same template under another verb is its own row, or local.
        assert_eq!(
            route_ceiling("GET", "/client/invites"),
            RequestCeiling::Local
        );
        assert_eq!(
            route_ceiling("POST", "/client/invites"),
            RequestCeiling::Long
        );
        assert_eq!(route_ceiling("GET", "/client/rules"), RequestCeiling::Local);
        assert_eq!(
            route_ceiling("POST", "/client/rules"),
            RequestCeiling::Upstream
        );
    }

    /// Every route the latency histogram charts as model time awaits a
    /// model before it answers, and so runs under the upstream ceiling.
    #[test]
    fn every_model_route_runs_under_the_upstream_ceiling() {
        for (method, template, class) in crate::http_metrics::ROUTE_CLASSES {
            if *class == squelch_core::metrics::RouteClass::Llm {
                assert_ne!(
                    route_ceiling(method, template),
                    RequestCeiling::Local,
                    "{method} {template}"
                );
            }
        }
    }

    #[test]
    fn the_ceilings_are_ordered_and_sit_above_the_client_wait() {
        assert!(REQUEST_TIMEOUT < UPSTREAM_REQUEST_TIMEOUT);
        assert!(UPSTREAM_REQUEST_TIMEOUT < LONG_REQUEST_TIMEOUT);
        // The Swift client idles out at 15 s (`APIClient.requestTimeout`).
        assert!(REQUEST_TIMEOUT > Duration::from_secs(15));
    }

    /// The long ceiling must clear the largest send: 25 MiB of attachments,
    /// base64 in 76-column CRLF lines (78 bytes on the wire per 57 in), the
    /// 2 MiB the JSON body may add, the `send_budget` that grows with all of
    /// that, the parent-headers fetch before it, the echo fetch after it,
    /// and a token refresh landing in the middle.
    #[test]
    fn the_long_ceiling_clears_the_largest_send() {
        let raw = crate::handlers::MAX_OUTBOUND_ATTACHMENT_BYTES * 78 / 57 + 2 * 1024 * 1024;
        let send = crate::write::send_budget(raw);
        let around = Duration::from_secs(60 + 15 + 5 + 5 + 30 + 5);
        assert!(
            LONG_REQUEST_TIMEOUT > send + around,
            "{LONG_REQUEST_TIMEOUT:?} does not clear {send:?} + {around:?}"
        );
    }

    #[test]
    fn the_upstream_ceiling_clears_one_gmail_call_and_a_refresh() {
        // 60 s request, 15 s connect, 5 s budget wait, 30 s token exchange,
        // and three store writes around it each waiting the 5 s busy handler.
        assert!(UPSTREAM_REQUEST_TIMEOUT > Duration::from_secs(60 + 15 + 5 + 30 + 3 * 5));
        // And the calendar writes' own 504 (`WRITE_DEADLINE`) fires first.
        assert!(UPSTREAM_REQUEST_TIMEOUT > crate::calendar_write::WRITE_DEADLINE);
    }
}
