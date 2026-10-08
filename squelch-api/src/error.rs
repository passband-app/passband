//! Error -> HTTP response mapping for the human door: errors become
//! `{"error": "..."}`, plus `"code"` where a route names its refusals in a
//! closed, machine-readable list (the calendar writes: docs/CALENDAR.md §8). Internal detail never leaks — every `CoreError` other
//! than `NotFound`/`InvalidInput` collapses to an opaque 500, and no token or
//! body ever reaches an error message.

use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::json;
use squelch_core::CoreError;

/// A handler error carrying an HTTP status and a client-safe message.
#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub message: String,
    /// A machine-readable name for the refusal, served as `"code"`. None
    /// serves no key. Only routes that document a closed list set one.
    pub code: Option<&'static str>,
}

impl ApiError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
            code: None,
        }
    }

    /// The same error with its machine-readable `code`.
    pub fn with_code(mut self, code: &'static str) -> Self {
        self.code = Some(code);
        self
    }

    /// 400 Bad Request with a client-facing message.
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }

    /// 404 Not Found (opaque; used for both missing and sealed-hidden).
    pub fn not_found() -> Self {
        Self::new(StatusCode::NOT_FOUND, "not found")
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = match self.code {
            Some(code) => json!({ "error": self.message, "code": code }),
            None => json!({ "error": self.message }),
        };
        (self.status, Json(body)).into_response()
    }
}

impl From<CoreError> for ApiError {
    fn from(e: CoreError) -> Self {
        match e {
            CoreError::NotFound => ApiError::not_found(),
            CoreError::InvalidInput(m) => ApiError::bad_request(m),
            // Opaque: never leak internal/store/credential detail across the wire.
            _ => ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal error"),
        }
    }
}
