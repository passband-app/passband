//! Calendar search (docs/CALENDAR.md §10.1), the two HTTP doors. Both sit in
//! the `calendar` feature's route group, so with the flag off they are 404.
//! Real events are read only while calendar is granted; ghosts always.
use crate::{ApiError, ApiState, handlers::store_call};
use axum::{
    Json,
    extract::{Query, State},
    http::header,
    response::IntoResponse,
};
use chrono::Utc;
use serde::Deserialize;
use serde_json::json;
use squelch_core::gcal::search::{
    AgentCalendarHit, Audience, CalendarHit, CalendarHits, CalendarQuery, search,
};

/// Hits served when the caller does not say.
const DEFAULT_LIMIT: usize = 20;

#[derive(Debug, Deserialize)]
pub struct CalendarSearchQuery {
    q: Option<String>,
    limit: Option<usize>,
}

/// Run one search for this door. A query no event can answer (mail-only
/// operators, no words) is an empty answer, not an error: the search box
/// sends every query here as the user types.
async fn run(
    state: &ApiState,
    query: CalendarSearchQuery,
    audience: Audience,
) -> Result<(CalendarHits, bool), ApiError> {
    let granted = match state.calendar() {
        Some(calendar) => calendar.granted().await,
        None => false,
    };
    let Some(parsed) = CalendarQuery::parse(query.q.as_deref().unwrap_or("")) else {
        return Ok((
            CalendarHits {
                hits: Vec::new(),
                total: 0,
                now_local: Utc::now().fixed_offset(),
            },
            granted,
        ));
    };
    let limit = query.limit.unwrap_or(DEFAULT_LIMIT);
    let now = Utc::now();
    let hits = store_call(state, move |store, account| {
        search(store, account, &parsed, audience, granted, now, limit)
    })
    .await?;
    Ok((hits, granted))
}

/// `GET /client/calendar/search?q=&limit=`: the search box's Events block.
/// HUMAN DOOR. Each item is exactly what `/client/timeline` serves for it,
/// under `event` or `ghost`.
pub async fn human(
    State(state): State<ApiState>,
    Query(query): Query<CalendarSearchQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let (found, granted) = run(&state, query, Audience::Owner).await?;
    let account_email = state
        .calendar()
        .map(|c| c.account_email().to_string())
        .unwrap_or_default();
    // The owner's clock: a ghost's state reads today in their zone.
    let now = found.now_local;
    let items: Vec<_> = found
        .hits
        .iter()
        .map(|hit| match hit {
            CalendarHit::Event(e) => {
                json!({ "event": crate::handlers::calendar_event_view(e, &account_email) })
            }
            CalendarHit::Ghost(g) => {
                let mut view = crate::handlers::ghost_view(g, now);
                // Search drops a ghost that matches a real event, so none
                // served here has one.
                view["matched_event"] = serde_json::Value::Null;
                json!({ "ghost": view })
            }
        })
        .collect();
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(json!({ "items": items, "total": found.total, "granted": granted })),
    ))
}

/// `GET /client/agent/calendar/search?q=&limit=`: the in-app assistant's
/// `search_calendar`. The agent view: no address of anyone, and a ghost only
/// with the threads the agent door may open.
pub async fn agent(
    State(state): State<ApiState>,
    Query(query): Query<CalendarSearchQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let (found, _) = run(&state, query, Audience::Agent).await?;
    // The owner's clock: a ghost's state reads today in their zone.
    let now = found.now_local;
    let results: Vec<AgentCalendarHit> = found
        .hits
        .iter()
        .map(|hit| AgentCalendarHit::from_hit(hit, now))
        .collect();
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(json!({ "results": results, "total": found.total })),
    ))
}
