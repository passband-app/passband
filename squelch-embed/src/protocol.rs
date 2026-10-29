//! THE WIRE CONTRACT between a hosted `squelchd` and `squelch-embed`.
//!
//! Both ends read these constants rather than restating them, because a
//! disagreement here does not fail loudly: a different model, dimension or
//! sequence length still returns 384 floats, they just stop being comparable
//! with the vectors already in a tenant's `message_vecs`. squelch-core pins its
//! own defaults to these in a test (`embed::remote`), so moving one without the
//! other fails CI rather than a mailbox.

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

/// Largest request body the server accepts, in bytes.
pub const MAX_BODY_BYTES: usize = 1024 * 1024;

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
/// 429: this request's lane is full; `Retry-After` says when to come back.
pub const ERR_QUEUE_FULL: &str = "queue_full";
/// 503: the session is not built yet.
pub const ERR_NOT_READY: &str = "not_ready";
/// 500: inference failed.
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

    #[test]
    fn the_batch_bound_is_the_daemon_default() {
        // docs/EMBED-SERVICE.md §2: the wire bound takes the daemon's number.
        assert_eq!(MAX_TEXTS_PER_CALL, 8);
    }
}
