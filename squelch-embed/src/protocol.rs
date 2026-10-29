//! THE WIRE CONTRACT between a hosted `squelchd` and `squelch-embed`.
//!
//! Both ends read these constants rather than restating them, because a
//! disagreement here does not fail loudly: a different model, dimension or
//! sequence length still returns 384 floats, they just stop being comparable
//! with the vectors already in a tenant's `message_vecs`. squelch-core pins its
//! own defaults to these in a test (`embed::remote`), so moving one without the
//! other fails CI rather than a mailbox.

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// The one model the service runs: fastembed's model code for BGE-small-en-v1.5,
/// fp32. Must equal squelch-core's `embed::DEFAULT_MODEL_CODE`.
pub const MODEL_CODE: &str = "Xenova/bge-small-en-v1.5";

/// Output width. Must equal the `float[N]` of `message_vecs` in squelch-core's
/// `store/schema.sql` and its `VEC_DIMS`.
pub const DIMS: usize = 384;

/// Tokens the model reads per text (fastembed `max_length`). Must equal
/// squelch-core's `embed::DEFAULT_EMBED_MAX_TOKENS`: `max_length` changes what
/// the model reads, so vectors made at two different values are in subtly
/// different spaces.
pub const MAX_TOKENS: usize = 256;

/// Most texts one `POST /embed` may carry. Equal to the daemon's default
/// `embed.backfill_batch` and to the server's inference batch, so a request is
/// never split (docs/EMBED-SERVICE.md §2). A caller with more chunks.
pub const MAX_TEXTS_PER_CALL: usize = 8;

/// Characters of one text the server reads; longer texts are TRUNCATED, never
/// rejected, because a search box is where someone pastes anything and a 400
/// there is a 500 in the client. A sanity bound, not a product decision: the
/// daemon already truncates ingest text to `embed.max_chars` (1000).
pub const MAX_CHARS_PER_TEXT: usize = 8192;

/// Largest request body the server accepts, in bytes. One text at
/// [`MAX_CHARS_PER_TEXT`] always fits, at its worst JSON escaping (six bytes a
/// character); a full call of such texts may not, and the daemon splits a call
/// that would not fit rather than send it.
pub const MAX_BODY_BYTES: usize = 256 * 1024;

/// The longest one text can be on the wire: every character a control
/// character JSON escapes as `\u00XX`, plus its quotes and comma.
pub const MAX_TEXT_WIRE_BYTES: usize = MAX_CHARS_PER_TEXT * 6 + 3;

// Room for one worst-case text and the `{"texts":[]}` around it, so a single
// text is never a request the server refuses for size.
const _: () = assert!(MAX_TEXT_WIRE_BYTES + 16 <= MAX_BODY_BYTES);

/// The daemon's whole-request timeout for an INTERACTIVE call: exactly one
/// text (a search query, one message at ingest) and the `/healthz` probe.
/// The server classifies a request by the same shape (one text is
/// interactive), so the two ends agree on which deadline applies.
pub const INTERACTIVE_TIMEOUT: Duration = Duration::from_secs(5);

/// The daemon's whole-request timeout for a BACKFILL call: two or more texts.
pub const BACKFILL_TIMEOUT: Duration = Duration::from_secs(30);

/// How long an interactive request may wait for its forward pass to START
/// before the server drops it unrun. Under [`INTERACTIVE_TIMEOUT`], so the
/// server never spends a pass on a caller that has already given up.
pub const INTERACTIVE_DEADLINE: Duration = Duration::from_millis(4500);

/// The backfill lane's deadline, under [`BACKFILL_TIMEOUT`] for the same
/// reason, with room for the pass itself: an 8-text pass at 256 tokens takes
/// 1 to 3 s on a shared core, so a pass that starts at the deadline still
/// answers inside the client's 30 s.
pub const BACKFILL_DEADLINE: Duration = Duration::from_secs(25);

const _: () = assert!(INTERACTIVE_DEADLINE.as_millis() < INTERACTIVE_TIMEOUT.as_millis());
const _: () = assert!(BACKFILL_DEADLINE.as_millis() < BACKFILL_TIMEOUT.as_millis());

/// The work listener: `POST /embed`. The only port the Service exposes.
pub const PORT: u16 = 8850;

/// The second listener, `/healthz` and `/metrics`, which no Service carries.
pub const METRICS_PORT: u16 = 9464;

/// The embedding route, on [`PORT`].
pub const EMBED_PATH: &str = "/embed";

/// Liveness and readiness. Served on [`PORT`] too (the daemon's one-shot
/// readiness probe goes through the Service) and on [`METRICS_PORT`] (the
/// kubelet's). 200 only once the session is built; 503 before.
pub const HEALTH_PATH: &str = "/healthz";

/// `POST /embed` body.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmbedRequest {
    /// 1..=[`MAX_TEXTS_PER_CALL`] texts. Positional: `vectors[i]` is `texts[i]`.
    pub texts: Vec<String>,
}

/// `200` body for `POST /embed`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EmbedResponse {
    /// Echoed on every response. The caller REJECTS a response whose `dims`, or
    /// any vector's length, disagrees with [`DIMS`] before it reaches a store.
    pub dims: usize,
    /// Same length and order as the request's `texts`.
    pub vectors: Vec<Vec<f32>>,
}

/// Body of every non-200 response. `error` is one of the closed words below,
/// never free text and never anything derived from the request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ErrorResponse {
    pub error: String,
}

/// 400: the request violated the shape (empty, too many texts, bad JSON).
pub const ERR_BAD_REQUEST: &str = "bad_request";
/// 401: missing or wrong bearer token.
pub const ERR_UNAUTHORIZED: &str = "unauthorized";
/// 429: this request's lane is full, the server is already reading as many
/// bodies as it will, or the request waited past its lane's deadline without
/// running; `Retry-After` says when to come back.
pub const ERR_QUEUE_FULL: &str = "queue_full";
/// 503: the session is not built yet.
pub const ERR_NOT_READY: &str = "not_ready";
/// 500: inference failed, or the inference thread stopped.
pub const ERR_INTERNAL: &str = "internal";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_round_trips_and_refuses_unknown_fields() {
        let r = EmbedRequest {
            texts: vec!["a".into(), "b".into()],
        };
        let s = serde_json::to_string(&r).unwrap();
        assert_eq!(s, r#"{"texts":["a","b"]}"#);
        assert_eq!(serde_json::from_str::<EmbedRequest>(&s).unwrap(), r);
        assert!(serde_json::from_str::<EmbedRequest>(r#"{"texts":[],"tenant":"x"}"#).is_err());
    }

    /// The server gives up on a request before the daemon does, so no pass
    /// is ever spent on a caller that has already timed out, and a body that
    /// fits one text is never refused for size.
    #[test]
    fn deadlines_sit_under_the_client_timeouts_and_a_text_fits_a_body() {
        assert_eq!(INTERACTIVE_TIMEOUT, Duration::from_secs(5));
        assert_eq!(BACKFILL_TIMEOUT, Duration::from_secs(30));
        assert_eq!(INTERACTIVE_DEADLINE, Duration::from_millis(4500));
        assert_eq!(BACKFILL_DEADLINE, Duration::from_secs(25));
        assert_eq!(MAX_BODY_BYTES, 256 * 1024);
        let worst = EmbedRequest {
            texts: vec!["\u{1}".repeat(MAX_CHARS_PER_TEXT)],
        };
        let wire = serde_json::to_vec(&worst).unwrap().len();
        assert!(wire <= MAX_TEXT_WIRE_BYTES + 16, "{wire}");
        assert!(wire <= MAX_BODY_BYTES, "{wire}");
    }

    #[test]
    fn the_batch_bound_is_the_daemon_default() {
        // docs/EMBED-SERVICE.md §2: the wire bound takes the daemon's number.
        assert_eq!(MAX_TEXTS_PER_CALL, 8);
    }
}
