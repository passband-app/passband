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
    self, Begin, ResearchResult, ResearchStatus, SenderResearchRow, Trigger,
};

use crate::error::ApiError;
use crate::handlers::{no_store, store_call};
use crate::state::ApiState;

#[derive(Debug, Deserialize)]
pub struct ResearchQuery {
    addr: String,
}

/// The card's view. `status` is `none` before anything was asked, `capped`
/// when today's on-demand runs are spent, and otherwise the row's own, except
/// that a running, failed or unavailable row that no longer stands against the
/// owner (a run left `running` by a restart once it is stale, a failure past
/// its short retry window) reads as `none`, so the button comes back.
#[derive(Debug, Serialize)]
pub struct ResearchView {
    pub status: &'static str,
    /// Whether the button can do anything here: research is on, a model is
    /// configured, and its wire has the web search tool.
    pub available: bool,
    pub researched_at: Option<String>,
    pub result: Option<ResearchResult>,
}

pub(crate) fn view(
    row: Option<SenderResearchRow>,
    available: bool,
    now: chrono::DateTime<Utc>,
    cache_days: i64,
) -> ResearchView {
    let lapsed = |row: &SenderResearchRow| {
        matches!(
            row.status,
            ResearchStatus::Running | ResearchStatus::Failed | ResearchStatus::Unavailable
        ) && !row.blocks(Trigger::OnDemand, now, cache_days)
    };
    match row.filter(|row| !lapsed(row)) {
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
    let cache_days = state.research_config.cache_days;
    Ok((
        no_store(),
        Json(view(row, available(&state), Utc::now(), cache_days)),
    ))
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
    let cache_days = state.research_config.cache_days;
    let Some(client) = state.sender_research.clone() else {
        let addr = addr.clone();
        let row = store_call(&state, move |store, account_id| {
            squelch_core::store::Store::sender_research(store, account_id, &addr)
        })
        .await?;
        return Ok((no_store(), Json(view(row, false, Utc::now(), cache_days))));
    };
    let cfg = state.research_config.clone();
    let begin_addr = addr.clone();
    let begin = store_call(&state, move |store, account_id| {
        research::begin_on_demand(store, &cfg, account_id, &begin_addr, Utc::now())
    })
    .await?;
    let subject = match begin {
        Begin::Run(subject) => subject,
        Begin::Standing(row) => {
            return Ok((
                no_store(),
                Json(view(Some(row), is_available, Utc::now(), cache_days)),
            ));
        }
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
    Ok((
        no_store(),
        Json(view(row, is_available, Utc::now(), cache_days)),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn row(status: ResearchStatus, requested: chrono::DateTime<Utc>) -> SenderResearchRow {
        SenderResearchRow {
            sender_addr: "pat@lumen.example".into(),
            status,
            trigger: Trigger::Auto,
            result: None,
            failure: None,
            web_searches: 0,
            requested_at: requested,
            researched_at: (status != ResearchStatus::Running).then_some(requested),
        }
    }

    /// A run a restart (or a store error) left `running` spins the card only
    /// until it is stale; then the button is back.
    #[test]
    fn a_stale_running_row_reads_as_nothing_so_the_button_returns() {
        let now = Utc::now();
        let fresh = row(ResearchStatus::Running, now - Duration::minutes(2));
        assert_eq!(view(Some(fresh), true, now, 90).status, "running");
        let stale = row(ResearchStatus::Running, now - Duration::hours(3));
        assert_eq!(view(Some(stale), true, now, 90).status, "none");
    }

    #[test]
    fn a_failure_reads_as_failed_only_inside_its_retry_window() {
        let now = Utc::now();
        let recent = row(ResearchStatus::Failed, now - Duration::minutes(5));
        assert_eq!(view(Some(recent), true, now, 90).status, "failed");
        let old = row(ResearchStatus::Failed, now - Duration::minutes(20));
        assert_eq!(view(Some(old), true, now, 90).status, "none");
        // A result stands for the cache window and is never hidden.
        let done = row(ResearchStatus::Done, now - Duration::days(200));
        assert_eq!(view(Some(done), true, now, 90).status, "done");
    }
}
