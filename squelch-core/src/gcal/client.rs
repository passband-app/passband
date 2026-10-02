//! The Calendar API client: `calendarList.list` and `events.list`, read only.
//!
//! Google's rules this follows, from the `events.list` reference and the
//! "Synchronize resources efficiently" guide:
//!
//! - `syncToken` cannot be combined with `timeMin`, `timeMax`, `updatedMin`,
//!   `orderBy`, `q`, `iCalUID` or the extended-property filters, so the window
//!   bounds only the first (full) request; an incremental page can carry any
//!   event, and the caller filters to the window.
//! - "Each list request should use the same set of query parameters,
//!   including the initial request": `singleEvents`, `maxResults`,
//!   `maxAttendees` and `fields` go on every request, full and incremental.
//! - `maxAttendees`: "If there are more than the specified number of
//!   attendees, only the participant is returned." At 1, an invitation with
//!   any number of guests comes back carrying the user's own entry alone,
//!   which is all [`super::read_event`] reads (its `self` and
//!   `responseStatus`).
//! - `fields` (partial response) names only what the daemon reads, so an
//!   invitation anyone can send cannot fill a page with its description and
//!   guest list, and a bloated page does not reach [`BODY_CAP`].
//! - `nextSyncToken` arrives on the LAST page only; every earlier page carries
//!   `nextPageToken`.
//! - 410 Gone means the sync token is invalid: clear it and sync in full.
//!
//! The base URL is injected, so tests point it at a local fake. Redirects are
//! refused (the request carries a bearer token), bodies are read up to a cap,
//! and an error carries a kind and never a body.

use std::time::Duration;

use serde_json::Value;

use super::CalendarErrorKind;

/// Production origin and path prefix.
pub const API_BASE: &str = "https://www.googleapis.com/calendar/v3";

const HTTP_TIMEOUT: Duration = Duration::from_secs(30);
const HTTP_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Events per page. Google's default; its maximum is 2500, and a smaller page
/// bounds what one response can make the daemon hold.
pub const EVENTS_PAGE_SIZE: u32 = 250;
/// Calendars per `calendarList` page: Google's maximum.
const LIST_PAGE_SIZE: u32 = 250;

/// Attendees per event: Google then returns only the user's own entry.
pub const MAX_ATTENDEES: u32 = 1;

/// The `events.list` partial response: the page's paging tokens and the
/// calendar's own title and zone, and of each event exactly what
/// [`super::read_event`] reads. Anything else an event carries (its
/// description, attachments, conference data, the other guests) is never
/// sent.
pub const EVENTS_FIELDS: &str = "nextPageToken,nextSyncToken,summary,timeZone,\
items(id,status,iCalUID,summary,location,start,end,organizer/email,\
attendees(self,responseStatus),transparency,htmlLink,updated)";

/// The `calendarList.list` partial response: what
/// [`super::read_calendar_entry`] reads.
pub const LIST_FIELDS: &str = "nextPageToken,\
items(id,summary,summaryOverride,primary,selected,deleted,hidden,timeZone)";

/// The most bytes read from one response. 250 events of the largest shape
/// Google sends stay far below it.
const BODY_CAP: usize = 8 * 1024 * 1024;
/// The most bytes read from an error body, which is only ever inspected for
/// Google's `reason`.
const ERROR_BODY_CAP: usize = 64 * 1024;

/// Why a call failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallError {
    /// 410: the sync token is no longer valid.
    Gone,
    /// 404: no such calendar (or no access to it any more).
    NotFound,
    Kind(CalendarErrorKind),
}

impl CallError {
    /// The kind stored and served. A 410 never reaches storage (the caller
    /// resyncs), and a calendar that is gone is unavailable.
    pub fn kind(self) -> CalendarErrorKind {
        match self {
            Self::Kind(kind) => kind,
            Self::Gone | Self::NotFound => CalendarErrorKind::Unavailable,
        }
    }
}

impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Gone => f.write_str("gone"),
            Self::NotFound => f.write_str("not_found"),
            Self::Kind(kind) => f.write_str(kind.as_str()),
        }
    }
}

/// One `events.list` page, items unparsed (each is read on its own, so one
/// odd item cannot fail the page).
#[derive(Debug, Default)]
pub struct EventsPage {
    pub items: Vec<Value>,
    pub next_page_token: Option<String>,
    pub next_sync_token: Option<String>,
    /// The calendar's own title and zone, which `events.list` carries: all the
    /// daemon learns about the primary calendar without the list scope.
    pub summary: Option<String>,
    pub time_zone: Option<String>,
}

/// One `calendarList.list` page.
#[derive(Debug, Default)]
pub struct ListPage {
    pub items: Vec<Value>,
    pub next_page_token: Option<String>,
}

/// What an `events.list` request asks for, apart from the page token.
#[derive(Debug, Clone)]
pub enum EventsQuery {
    /// A full sync of the window.
    Full {
        time_min: chrono::DateTime<chrono::Utc>,
        time_max: chrono::DateTime<chrono::Utc>,
    },
    /// Changes since the token.
    Since(String),
}

#[derive(Clone)]
pub struct CalendarApi {
    http: reqwest::Client,
    base: String,
}

impl CalendarApi {
    /// A client against `base` (production: [`API_BASE`]).
    pub fn new(base: impl Into<String>) -> reqwest::Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(HTTP_TIMEOUT)
            .connect_timeout(HTTP_CONNECT_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self {
            http,
            base: base.into(),
        })
    }

    fn url(&self, segments: &[&str]) -> Result<url::Url, CallError> {
        let mut url = url::Url::parse(&self.base)
            .map_err(|_| CallError::Kind(CalendarErrorKind::Unavailable))?;
        url.path_segments_mut()
            .map_err(|_| CallError::Kind(CalendarErrorKind::Unavailable))?
            .pop_if_empty()
            .extend(segments);
        Ok(url)
    }

    /// One page of the user's calendar list. Needs the list scope.
    pub async fn calendar_list(
        &self,
        token: &str,
        page_token: Option<&str>,
    ) -> Result<ListPage, CallError> {
        let mut url = self.url(&["users", "me", "calendarList"])?;
        {
            let mut q = url.query_pairs_mut();
            q.append_pair("maxResults", &LIST_PAGE_SIZE.to_string());
            q.append_pair("fields", LIST_FIELDS);
            if let Some(page) = page_token {
                q.append_pair("pageToken", page);
            }
        }
        let body = self.get(url, token).await?;
        Ok(ListPage {
            items: items(&body),
            next_page_token: string(&body, "nextPageToken"),
        })
    }

    /// One page of a calendar's events, recurring events expanded into their
    /// instances (`singleEvents`). The calendar id is a path segment and is
    /// percent-encoded as one (ids carry `@` and `#`).
    pub async fn events(
        &self,
        token: &str,
        calendar_id: &str,
        query: &EventsQuery,
        page_token: Option<&str>,
    ) -> Result<EventsPage, CallError> {
        let mut url = self.url(&["calendars", calendar_id, "events"])?;
        {
            let mut q = url.query_pairs_mut();
            q.append_pair("singleEvents", "true");
            q.append_pair("maxResults", &EVENTS_PAGE_SIZE.to_string());
            q.append_pair("maxAttendees", &MAX_ATTENDEES.to_string());
            q.append_pair("fields", EVENTS_FIELDS);
            match query {
                EventsQuery::Full { time_min, time_max } => {
                    q.append_pair("timeMin", &time_min.to_rfc3339());
                    q.append_pair("timeMax", &time_max.to_rfc3339());
                }
                EventsQuery::Since(sync_token) => {
                    q.append_pair("syncToken", sync_token);
                }
            }
            if let Some(page) = page_token {
                q.append_pair("pageToken", page);
            }
        }
        let body = self.get(url, token).await?;
        Ok(EventsPage {
            items: items(&body),
            next_page_token: string(&body, "nextPageToken"),
            next_sync_token: string(&body, "nextSyncToken"),
            summary: string(&body, "summary"),
            time_zone: string(&body, "timeZone"),
        })
    }

    async fn get(&self, url: url::Url, token: &str) -> Result<Value, CallError> {
        let unavailable = CallError::Kind(CalendarErrorKind::Unavailable);
        let resp = self
            .http
            .get(url)
            .bearer_auth(token)
            .send()
            .await
            .map_err(|_| unavailable)?;
        let status = resp.status().as_u16();
        if status == 200 {
            let bytes = read_capped(resp, BODY_CAP).await.ok_or(unavailable)?;
            let body: Value = serde_json::from_slice(&bytes).map_err(|_| unavailable)?;
            return if body.is_object() {
                Ok(body)
            } else {
                Err(unavailable)
            };
        }
        Err(match status {
            401 => CallError::Kind(CalendarErrorKind::Auth),
            403 => {
                let body = read_capped(resp, ERROR_BODY_CAP).await.unwrap_or_default();
                CallError::Kind(forbidden_kind(&body))
            }
            404 => CallError::NotFound,
            410 => CallError::Gone,
            429 => CallError::Kind(CalendarErrorKind::Quota),
            _ => unavailable,
        })
    }
}

/// A 403 is a rate limit when Google's `reason` says so, and otherwise a
/// permission problem: the token lacks the scope, or the user cannot read
/// that calendar.
fn forbidden_kind(body: &[u8]) -> CalendarErrorKind {
    const RATE: [&str; 4] = [
        "rateLimitExceeded",
        "userRateLimitExceeded",
        "quotaExceeded",
        "dailyLimitExceeded",
    ];
    let body: Value = serde_json::from_slice(body).unwrap_or_default();
    let rate_limited = body
        .pointer("/error/errors")
        .and_then(Value::as_array)
        .is_some_and(|errors| {
            errors.iter().any(|e| {
                e.get("reason")
                    .and_then(Value::as_str)
                    .is_some_and(|r| RATE.contains(&r))
            })
        });
    if rate_limited {
        CalendarErrorKind::Quota
    } else {
        CalendarErrorKind::Auth
    }
}

/// The body, or None past `cap` bytes or on a transport error mid-body.
async fn read_capped(mut resp: reqwest::Response, cap: usize) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    while let Some(chunk) = resp.chunk().await.ok()? {
        if out.len() + chunk.len() > cap {
            return None;
        }
        out.extend_from_slice(&chunk);
    }
    Some(out)
}

fn items(body: &Value) -> Vec<Value> {
    body.get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn string(body: &Value, key: &str) -> Option<String> {
    body.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_403_is_quota_only_when_google_says_rate_limit() {
        let rate = json!({"error": {"code": 403, "errors": [{"reason": "rateLimitExceeded"}]}});
        assert_eq!(
            forbidden_kind(rate.to_string().as_bytes()),
            CalendarErrorKind::Quota
        );
        let scope = json!({"error": {"code": 403, "errors": [{"reason": "insufficientPermissions"}],
                           "status": "PERMISSION_DENIED"}});
        assert_eq!(
            forbidden_kind(scope.to_string().as_bytes()),
            CalendarErrorKind::Auth
        );
        assert_eq!(forbidden_kind(b"not json"), CalendarErrorKind::Auth);
    }

    #[test]
    fn a_calendar_id_is_one_encoded_path_segment() {
        let api = CalendarApi::new("http://127.0.0.1:9/calendar/v3").unwrap();
        let url = api
            .url(&["calendars", "team#x@group.calendar.google.com", "events"])
            .unwrap();
        assert_eq!(
            url.path(),
            "/calendar/v3/calendars/team%23x@group.calendar.google.com/events"
        );
        let api = CalendarApi::new("http://127.0.0.1:9").unwrap();
        assert_eq!(api.url(&["users", "me"]).unwrap().path(), "/users/me");
    }
}
