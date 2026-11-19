//! `/client/person/research`: the contact card's "Who is this?"
//! (docs/SENDER-RESEARCH.md). GET reads what research has on one sender; POST
//! runs it now, once, unless a result already stands.
//!
//! HUMAN DOOR ONLY, like `/client/person`: what research says about a
//! stranger is UNTRUSTED model output for the owner to read, and the agent
//! door has no route to it (`squelch-mcp/tests/research_never_crosses.rs`).

use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use squelch_core::triage::research::{
    self, Begin, ResearchResult, ResearchStatus, SenderResearchRow,
};

use crate::error::ApiError;
use crate::handlers::{no_store, store_call};
use crate::state::ApiState;

#[derive(Debug, Deserialize)]
pub struct ResearchQuery {
    addr: String,
}

/// The card's view. `status` is `none` before anything was asked, `capped`
/// when today's on-demand runs are spent, and otherwise the row's own.
#[derive(Debug, Serialize)]
pub struct ResearchView {
    pub status: &'static str,
    /// Whether the button can do anything here: research is on, a model is
    /// configured, and its wire has the web search tool.
    pub available: bool,
    pub researched_at: Option<String>,
    pub result: Option<ResearchResult>,
}

fn view(row: Option<SenderResearchRow>, available: bool) -> ResearchView {
    match row {
        None => ResearchView {
            status: "none",
            available,
            researched_at: None,
            result: None,
        },
        Some(row) => ResearchView {
            status: row.status.as_str(),
            available,
            researched_at: row.researched_at.map(|at| at.to_rfc3339()),
            result: (row.status == ResearchStatus::Done)
                .then_some(row.result)
                .flatten(),
        },
    }
}

fn valid_addr(addr: &str) -> Result<String, ApiError> {
    let addr = addr.trim().to_lowercase();
    if addr.is_empty() || !addr.contains('@') || addr.len() > 320 {
        return Err(ApiError::bad_request("addr must be an email address"));
    }
    Ok(addr)
}

fn available(state: &ApiState) -> bool {
    state
        .sender_research
        .as_ref()
        .is_some_and(research::ResearchClient::available)
}

/// GET: a store read. Never runs research.
pub async fn get_research(
    State(state): State<ApiState>,
    Query(params): Query<ResearchQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let addr = valid_addr(&params.addr)?;
    let row = store_call(&state, move |store, account_id| {
        squelch_core::store::Store::sender_research(store, account_id, &addr)
    })
    .await?;
    Ok((no_store(), Json(view(row, available(&state)))))
}

/// POST: run research on this sender now, and answer with the result. A
/// result that stands (inside the cache window, or a run already in flight)
/// is answered as it is, at no cost.
pub async fn run_research(
    State(state): State<ApiState>,
    Query(params): Query<ResearchQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let addr = valid_addr(&params.addr)?;
    let is_available = available(&state);
    let Some(client) = state.sender_research.clone() else {
        let addr = addr.clone();
        let row = store_call(&state, move |store, account_id| {
            squelch_core::store::Store::sender_research(store, account_id, &addr)
        })
        .await?;
        return Ok((no_store(), Json(view(row, false))));
    };
    let cfg = state.research_config.clone();
    let begin_addr = addr.clone();
    let begin = store_call(&state, move |store, account_id| {
        research::begin_on_demand(store, &cfg, account_id, &begin_addr, Utc::now())
    })
    .await?;
    let subject = match begin {
        Begin::Run(subject) => subject,
        Begin::Standing(row) => return Ok((no_store(), Json(view(Some(row), is_available)))),
        Begin::Capped => {
            return Ok((
                no_store(),
                Json(ResearchView {
                    status: "capped",
                    available: is_available,
                    researched_at: None,
                    result: None,
                }),
            ));
        }
        Begin::Refused => return Err(ApiError::bad_request("this address cannot be researched")),
    };
    // Spawned, so a card closed mid-call still records what was paid for
    // rather than leaving the row running until it goes stale.
    let finish_state = state.clone();
    let row = tokio::spawn(async move {
        let call = client.research(&subject).await;
        store_call(&finish_state, move |store, account_id| {
            research::finish(store, account_id, &addr, &call, Utc::now())
        })
        .await
    })
    .await
    .map_err(|_| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal error"))??;
    Ok((no_store(), Json(view(row, is_available))))
}
