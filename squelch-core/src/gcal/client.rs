//! The Calendar API client: `calendarList.list` and `events.list` for the
//! sync, and `events.insert`, `events.patch` and `events.delete` for the
//! user's own writes from the human door.
//!
//! EVERY WRITE NAMES `sendUpdates`, and the value is [`Notify`]'s: `none`
//! ("No notifications are sent") unless the caller passed [`Notify::Guests`],
//! which sends `all` ("Notifications are sent to all guests"). Those are the
//! only two values this client can put on the wire, and `Notify::default()`
//! is the silent one, so a caller that says nothing mails nobody. The mail is
//! Google's own invitation, update or cancellation; nothing here writes one.
//! An all-day event's `end.date` is
//! exclusive at Google (the Events resource: `end` is "The (exclusive) end
//! time of the event"); the conversion to and from the inclusive last day the
//! app uses lives in [`super::write`] and [`super::read_event`].
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
//!   attendees, only the participant is returned." At [`MAX_ATTENDEES`] an
//!   event's whole guest list comes back when it has that many or fewer, and
//!   a larger one comes back as the user's own entry alone with
//!   `attendeesOmitted` set, which is stored as the event's truncated mark.
//!   Either way [`super::read_event`] still finds the user's own answer.
//! - `fields` (partial response) names only what the daemon reads, so an
//!   invitation anyone can send cannot fill a page with attachments,
//!   conference data or its guests' comments: of each attendee only the
//!   address, the name, the answer and the two flags are asked for. Its
//!   description is read (the app shows it), laundered and cut to 2000
//!   characters as it is stored.
//! - `events.patch`: "Array fields, if specified, overwrite the existing
//!   arrays; this discards any previous array elements." So a guest edit
//!   sends the FULL attendee list, built from a fresh read of the event
//!   ([`CalendarApi::event_attendees`]), never from a partial one.
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

/// Attendees per event ([`super::MAX_GUESTS`]): past it Google returns only
/// the user's own entry and marks the event `attendeesOmitted`.
pub const MAX_ATTENDEES: u32 = super::MAX_GUESTS as u32;

/// Of one event, exactly what [`super::read_event`] reads. Anything else an
/// event carries (attachments, conference data, what a guest wrote when
/// answering) is never sent. One macro so a listed event and a written one
/// are asked for alike.
macro_rules! event_fields {
    () => {
        "id,status,iCalUID,summary,description,location,colorId,start,end,\
organizer(email,self),attendeesOmitted,\
attendees(email,displayName,responseStatus,organizer,self),transparency,htmlLink,updated"
    };
}

/// The `events.list` partial response: the page's paging tokens and the
/// calendar's own title and zone, and of each event [`EVENT_FIELDS`].
pub const EVENTS_FIELDS: &str = concat!(
    "nextPageToken,nextSyncToken,summary,timeZone,items(",
    event_fields!(),
    ")"
);

/// The partial response of `events.insert` and `events.patch`: the one event
/// written, read by the same [`super::read_event`].
pub const EVENT_FIELDS: &str = event_fields!();

/// The `calendarList.list` partial response: what
/// [`super::read_calendar_entry`] reads.
pub const LIST_FIELDS: &str = "nextPageToken,\
items(id,summary,summaryOverride,primary,selected,deleted,hidden,timeZone,\
backgroundColor,colorId,accessRole)";

/// The partial response of the read a guest edit starts with
/// ([`CalendarApi::event_attendees`]): the attendee entries WHOLE, every
/// field Google holds about each, because the patch that follows hands the
/// kept ones back and whatever it left out Google would drop. Held in memory
/// for that one request, never stored, logged or served.
pub const ATTENDEES_FIELDS: &str = "attendeesOmitted,attendees";

/// WHO GOOGLE TELLS about a write: the `sendUpdates` parameter. The ONLY two
/// values this client sends; `externalOnly` is not offered.
///
/// THE DEFAULT IS SILENCE. `Guests` makes Google email every guest of the
/// event (an invitation, an update or a cancellation), which cannot be
/// unsent, so it is only ever built from a request that said `notify: true`
/// ([`super::write::notify`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Notify {
    /// `sendUpdates=none`: "No notifications are sent."
    #[default]
    Nobody,
    /// `sendUpdates=all`: "Notifications are sent to all guests."
    Guests,
}

impl Notify {
    /// The value on the wire.
    pub fn send_updates(self) -> &'static str {
        match self {
            Self::Nobody => "none",
            Self::Guests => "all",
        }
    }
    /// Whether guests are told.
    pub fn tells_guests(self) -> bool {
        self == Self::Guests
    }
}

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
    /// 409: an insert named an event id the calendar already holds (Google's
    /// `duplicate`, "The requested identifier already exists").
    Duplicate,
    /// The request went out and no usable answer came back: the connection
    /// was lost or timed out after sending, a 2xx body could not be read, or
    /// Google answered 5xx (its own failure, which does not say the request
    /// was not applied). Google may have done what was asked. A write that ends here is NOT a
    /// refusal, and is never retried.
    Uncertain,
    Kind(CalendarErrorKind),
}

impl CallError {
    /// The kind stored and served. A 410 never reaches storage (the caller
    /// resyncs), and a calendar that is gone is unavailable.
    pub fn kind(self) -> CalendarErrorKind {
        match self {
            Self::Kind(kind) => kind,
            Self::Gone | Self::NotFound | Self::Duplicate | Self::Uncertain => {
                CalendarErrorKind::Unavailable
            }
        }
    }
}

impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Gone => f.write_str("gone"),
            Self::NotFound => f.write_str("not_found"),
            Self::Duplicate => f.write_str("duplicate"),
            Self::Uncertain => f.write_str("uncertain"),
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

    /// The base plus `segments`, each one percent-encoded path segment. An
    /// empty, `.` or `..` segment is refused: the `url` crate drops the first
    /// two and pops on the third instead of encoding them, so an event id
    /// `..` would turn a write on one event into a call on the collection.
    fn url(&self, segments: &[&str]) -> Result<url::Url, CallError> {
        if segments.iter().any(|s| matches!(*s, "" | "." | "..")) {
            return Err(CallError::NotFound);
        }
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

    /// The attendees of one event as Google holds them now, each entry whole
    /// ([`ATTENDEES_FIELDS`]), and whether Google left any out. What a guest
    /// edit builds its full list from: the stored copy is minutes old, and a
    /// guest who answered since would have that answer overwritten by it.
    pub async fn event_attendees(
        &self,
        token: &str,
        calendar_id: &str,
        event_id: &str,
    ) -> Result<(Vec<Value>, bool), CallError> {
        let mut url = self.url(&["calendars", calendar_id, "events", event_id])?;
        url.query_pairs_mut()
            .append_pair("maxAttendees", &MAX_ATTENDEES.to_string())
            .append_pair("fields", ATTENDEES_FIELDS);
        let body = self.get(url, token).await?;
        let attendees = body
            .get("attendees")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let omitted = body.get("attendeesOmitted").and_then(Value::as_bool) == Some(true);
        Ok((attendees, omitted))
    }

    /// `events.get` of one event, in the shape a write's answer has
    /// ([`EVENT_FIELDS`], the same attendee bound): what a create that names
    /// its own id reads first, so a create sent twice is answered with the
    /// event the first one made instead of a second insert.
    pub async fn get_event(
        &self,
        token: &str,
        calendar_id: &str,
        event_id: &str,
    ) -> Result<Value, CallError> {
        let mut url = self.url(&["calendars", calendar_id, "events", event_id])?;
        url.query_pairs_mut()
            .append_pair("maxAttendees", &MAX_ATTENDEES.to_string())
            .append_pair("fields", EVENT_FIELDS);
        self.get(url, token).await
    }

    /// `calendarList.get`: whether the calendar is still on the user's
    /// calendar list, not marked `deleted`, AND still one the user writes to
    /// (`accessRole` `owner` or `writer`). A 404 is NO: the user has lost the
    /// calendar. So is a calendar still listed whose role dropped to a
    /// reader's: an event the user can no longer see is a 404 to them, and
    /// that is not "already deleted". Needs the list scope, which every
    /// calendar other than the primary one is read with.
    pub async fn calendar_listed(&self, token: &str, calendar_id: &str) -> Result<bool, CallError> {
        let mut url = self.url(&["users", "me", "calendarList", calendar_id])?;
        url.query_pairs_mut()
            .append_pair("fields", "id,deleted,accessRole");
        match self.get(url, token).await {
            Ok(entry) => Ok(entry.get("deleted").and_then(Value::as_bool) != Some(true)
                && matches!(
                    entry.get("accessRole").and_then(Value::as_str),
                    Some("owner" | "writer")
                )),
            Err(CallError::NotFound | CallError::Gone) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// `events.insert`: create `event` (Google's event shape) on the calendar.
    /// Google's answer, the event as stored, carries its id, its link and the
    /// times as Google normalized them. With [`Notify::Guests`] Google mails
    /// an invitation to each attendee in `event`.
    pub async fn insert_event(
        &self,
        token: &str,
        calendar_id: &str,
        event: &Value,
        notify: Notify,
    ) -> Result<Value, CallError> {
        let url = self.write_url(&["calendars", calendar_id, "events"], notify, true)?;
        self.call(reqwest::Method::POST, url, token, Some(event))
            .await
    }

    /// `events.patch`: "field values you specify replace the existing
    /// values. Fields that you don't specify in the request remain
    /// unchanged", and a field set to `null` is deleted. An `attendees` array
    /// in it REPLACES the event's. With [`Notify::Guests`] Google mails the
    /// event's guests about the change.
    pub async fn patch_event(
        &self,
        token: &str,
        calendar_id: &str,
        event_id: &str,
        patch: &Value,
        notify: Notify,
    ) -> Result<Value, CallError> {
        let url = self.write_url(
            &["calendars", calendar_id, "events", event_id],
            notify,
            true,
        )?;
        self.call(reqwest::Method::PATCH, url, token, Some(patch))
            .await
    }

    /// `events.delete`. Success is "an empty response body". A 410 (the event
    /// is already deleted at Google) is [`CallError::Gone`], which the caller
    /// reads as done. With [`Notify::Guests`] Google mails the event's guests
    /// a cancellation.
    pub async fn delete_event(
        &self,
        token: &str,
        calendar_id: &str,
        event_id: &str,
        notify: Notify,
    ) -> Result<(), CallError> {
        let url = self.write_url(
            &["calendars", calendar_id, "events", event_id],
            notify,
            false,
        )?;
        self.call(reqwest::Method::DELETE, url, token, None)
            .await
            .map(|_| ())
    }

    /// A write's URL. `sendUpdates` is ALWAYS named (never left to Google's
    /// default) and is `notify`'s value; when an event comes back, the
    /// one-event fields mask and the same attendee bound the sync uses.
    fn write_url(
        &self,
        segments: &[&str],
        notify: Notify,
        answers: bool,
    ) -> Result<url::Url, CallError> {
        let mut url = self.url(segments)?;
        {
            let mut q = url.query_pairs_mut();
            q.append_pair("sendUpdates", notify.send_updates());
            if answers {
                q.append_pair("maxAttendees", &MAX_ATTENDEES.to_string());
                q.append_pair("fields", EVENT_FIELDS);
            }
        }
        Ok(url)
    }

    /// A read, whose answer must be a JSON object: an empty body is Google
    /// misbehaving, never an empty page (a full read would sweep on it).
    async fn get(&self, url: url::Url, token: &str) -> Result<Value, CallError> {
        match self.call(reqwest::Method::GET, url, token, None).await? {
            Value::Null => Err(CallError::Kind(CalendarErrorKind::Unavailable)),
            body => Ok(body),
        }
    }

    /// One request. A 2xx answers its JSON object, or `Null` for an empty
    /// body (a delete). Never retried: a write that timed out may have
    /// happened, and sending it again could make it twice. A failure before
    /// the request could have reached Google (no connection) is
    /// `Unavailable`; one after (a timeout or a lost connection once
    /// connected, a 2xx whose body does not read, any 5xx from Google) is
    /// [`CallError::Uncertain`]. A 4xx is Google's refusal, by its status.
    /// A read treats both alike ([`CallError::kind`]).
    async fn call(
        &self,
        method: reqwest::Method,
        url: url::Url,
        token: &str,
        body: Option<&Value>,
    ) -> Result<Value, CallError> {
        let unavailable = CallError::Kind(CalendarErrorKind::Unavailable);
        let mut request = self.http.request(method, url).bearer_auth(token);
        if let Some(body) = body {
            request = request.json(body);
        }
        let resp = request.send().await.map_err(|e| {
            if e.is_connect() {
                unavailable
            } else {
                CallError::Uncertain
            }
        })?;
        let status = resp.status().as_u16();
        if (200..300).contains(&status) {
            let uncertain = CallError::Uncertain;
            let bytes = read_capped(resp, BODY_CAP).await.ok_or(uncertain)?;
            if bytes.iter().all(u8::is_ascii_whitespace) {
                return Ok(Value::Null);
            }
            let body: Value = serde_json::from_slice(&bytes).map_err(|_| uncertain)?;
            return if body.is_object() {
                Ok(body)
            } else {
                Err(uncertain)
            };
        }
        Err(match status {
            401 => CallError::Kind(CalendarErrorKind::Auth),
            403 => {
                let body = read_capped(resp, ERROR_BODY_CAP).await.unwrap_or_default();
                CallError::Kind(forbidden_kind(&body))
            }
            404 => CallError::NotFound,
            409 => CallError::Duplicate,
            410 => CallError::Gone,
            429 => CallError::Kind(CalendarErrorKind::Quota),
            // GOOGLE'S OWN 5xx IS NOT A REFUSAL. Its error guide says a 500 is
            // "an unexpected error occurred while processing the request", and
            // a 502 or 503 from its front end says less still: a write that
            // got one may have been applied (verify5 Y1). A read reports it as
            // unavailable, exactly as before ([`CallError::kind`]).
            500..=599 => CallError::Uncertain,
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
        // A dot segment, or none, is refused rather than collapsed: `..` would
        // otherwise make `calendars/primary/events/..` the events collection.
        for bad in ["..", ".", ""] {
            assert_eq!(
                api.url(&["calendars", "primary", "events", bad]),
                Err(CallError::NotFound),
                "{bad:?}"
            );
            assert_eq!(
                api.url(&["calendars", bad, "events"]),
                Err(CallError::NotFound)
            );
        }
        assert_eq!(
            api.url(&["calendars", "primary", "events", "..."])
                .unwrap()
                .path(),
            "/calendars/primary/events/..."
        );
    }

    /// THE NOTIFY DEFAULT: a write that was told nothing tells nobody, and
    /// the wire only ever carries `none` or `all`.
    #[test]
    fn a_write_tells_nobody_unless_asked_and_always_says_which() {
        assert_eq!(Notify::default(), Notify::Nobody);
        assert_eq!(Notify::default().send_updates(), "none");
        assert_eq!(Notify::Guests.send_updates(), "all");
        assert!(!Notify::Nobody.tells_guests() && Notify::Guests.tells_guests());
        let api = CalendarApi::new("http://127.0.0.1:9").unwrap();
        let sent = |notify, answers| {
            let url = api
                .write_url(&["calendars", "primary", "events"], notify, answers)
                .unwrap();
            let pairs: Vec<(String, String)> = url.query_pairs().into_owned().collect();
            let values: Vec<String> = pairs
                .iter()
                .filter(|(key, _)| key == "sendUpdates")
                .map(|(_, value)| value.clone())
                .collect();
            assert!(!pairs.iter().any(|(key, _)| key == "sendNotifications"));
            values
        };
        for answers in [true, false] {
            assert_eq!(sent(Notify::default(), answers), ["none"]);
            assert_eq!(sent(Notify::Nobody, answers), ["none"]);
            assert_eq!(sent(Notify::Guests, answers), ["all"]);
        }
    }
}
