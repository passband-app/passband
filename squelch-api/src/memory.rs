//! `/client/memory`: Settings, "What Passband knows about you"
//! (docs/UNDERSTANDING.md §6). Every entry the profile pass inferred or the
//! owner stated, with its receipts; confirm, edit, delete.
//!
//! HUMAN DOOR ONLY, and there will never be an `/mcp` counterpart: a profile
//! of the owner and the people they answer is the graph the agent door was
//! never handed (an invariant test in `squelch-mcp` pins that no tool
//! response carries any of it).
//!
//! BEHIND THE `understanding` HIDDEN FEATURE ([`squelch_core::features::
//! UNDERSTANDING`]). Off is as if the routes did not exist: every one answers
//! the opaque 404, from a route layer that runs before any extractor, so a
//! refused request has its body read by nobody and writes no audit row (the
//! calendar routes' gate, for the same reasons).
//!
//! Writes are AUDITED (`memory.update`, `memory.delete`), the detail naming
//! what kind of write it was and never the entry's words: the audit log is
//! read back on surfaces that have no business re-listing a profile.

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    middleware,
    response::IntoResponse,
};
use chrono::Utc;
use serde::Deserialize;
use serde_json::json;
use squelch_core::store::memory::MemoryStore as _;
use squelch_core::store::sqlite::memory::MemoryEdit;
use squelch_core::triage::memory::is_ulid;
use squelch_core::triage::sender_fatigue::MIN_COVERED_DAYS;

use crate::error::ApiError;
use crate::handlers::{audit_action, store_call};
use crate::state::ApiState;

/// `GET /client/memory`: every entry not deleted, newest first, retired ones
/// included so the page can say what Passband no longer thinks.
///
/// `enabled` is whether the nightly pass is running for this mailbox yet: the
/// dwell ledger covers `MIN_COVERED_DAYS`. Under that the page shows its
/// "still learning" state with `coverage_days`, and the list holds only what
/// the owner stated.
pub async fn list_memory(State(state): State<ApiState>) -> Result<impl IntoResponse, ApiError> {
    let (coverage, entries) = store_call(&state, |store, account_id| {
        Ok((
            store.memory_coverage_days(account_id, Utc::now())?,
            store.list_memory_entries(account_id)?,
        ))
    })
    .await?;
    Ok(Json(json!({
        "enabled": coverage >= MIN_COVERED_DAYS,
        "coverage_days": coverage.floor() as i64,
        "entries": entries,
    })))
}

#[derive(Debug, Deserialize)]
pub struct MemoryBody {
    /// The owner's own sentence for the entry.
    #[serde(default)]
    line: Option<String>,
    /// New fields for the entry's kind.
    #[serde(default)]
    body: Option<serde_json::Value>,
    /// "That's right": the entry becomes the owner's word.
    #[serde(default)]
    confirm: bool,
}

/// `PUT /client/memory/{id}`: confirm or edit one entry. Either makes it
/// `stated` (see `SqliteStore::edit_memory_entry` for why an edit does too).
pub async fn update_memory(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Json(body): Json<MemoryBody>,
) -> Result<impl IntoResponse, ApiError> {
    // Not an id we mint: the same 404 as an id we do not hold, before the
    // client's text reaches the store or the audit log.
    if !is_ulid(&id) {
        return Err(ApiError::not_found());
    }
    let edit = MemoryEdit {
        line: body.line,
        body: body.body,
        confirm: body.confirm,
    };
    let detail = match (edit.line.is_some() || edit.body.is_some(), edit.confirm) {
        (true, _) => "edited",
        (false, true) => "confirmed",
        (false, false) => "unchanged",
    };
    let target = Some(id.clone());
    let result = store_call(&state, move |store, account_id| {
        store.edit_memory_entry(account_id, &id, &edit, Utc::now())
    })
    .await;
    match result {
        Ok(Some(entry)) => {
            if detail != "unchanged" {
                audit_action(&state, "memory.update", target, detail).await;
            }
            Ok(Json(entry))
        }
        Ok(None) => Err(ApiError::not_found()),
        Err(e) => {
            audit_action(&state, "memory.update", target, "rejected").await;
            Err(e)
        }
    }
}

/// `DELETE /client/memory/{id}`: the owner removes an entry. Never served to
/// anything again; the row stays behind `deleted_at` so the audit row points
/// at something.
pub async fn delete_memory(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    if !is_ulid(&id) {
        return Err(ApiError::not_found());
    }
    let target = id.clone();
    let deleted = store_call(&state, move |store, account_id| {
        store.delete_memory_entry(account_id, &id, Utc::now())
    })
    .await?;
    if !deleted {
        return Err(ApiError::not_found());
    }
    audit_action(&state, "memory.delete", Some(target), "deleted").await;
    Ok(StatusCode::NO_CONTENT)
}

/// The route table, mounted into the bearer-authed `/client` tree, every
/// route behind [`require_understanding`].
pub fn routes(state: ApiState) -> Router<ApiState> {
    use axum::routing::get;
    Router::new()
        .route("/client/memory", get(list_memory))
        .route(
            "/client/memory/{id}",
            axum::routing::put(update_memory).delete(delete_memory),
        )
        .route_layer(middleware::from_fn_with_state(state, require_understanding))
}

/// The `understanding` gate. See the module header.
async fn require_understanding(
    State(state): State<ApiState>,
    req: axum::extract::Request,
    next: middleware::Next,
) -> axum::response::Response {
    if !state
        .features()
        .enabled(squelch_core::features::UNDERSTANDING)
    {
        return ApiError::not_found().into_response();
    }
    next.run(req).await
}
