//! AN EVENT'S ONLINE MEETING, and the Join button on its card.
//!
//! THE JOIN LINK IS NEVER STORED. A meeting link's query (and some paths)
//! carries the passcode, and the stored and served copy of every event is
//! laundered so that it never holds one (`ghost::launder_location`,
//! `ghost::launder_description`). So the sync keeps only WHICH service the
//! meeting is on ([`MeetingKind`], from [`meeting_kind`]), enough for the app
//! to draw "Join Zoom"; the link itself is read from Google when the user
//! presses Join ([`join_url`], on the human door's
//! `GET /client/calendar/events/{calendar_id}/{id}/join`), handed to the app
//! to open, and kept nowhere.
//!
//! Where a meeting is found, first that holds one:
//! 1. Google's conference (`conferenceData.entryPoints`, the `video` entry):
//!    Meet, or a Zoom or Teams add-on.
//! 2. `hangoutLink`, Google's own Meet link on older events.
//! 3. A link in the location, then in the description, on a host known to
//!    run meetings ([`kind_of_host`]). Only those: a description is written by
//!    whoever sent the invitation, and Join must not open just any link in it.
//!
//! MAKING A MEET. A create or an edit may ask Google for a new Meet
//! ([`create_request`], sent with `conferenceDataVersion=1`, which every
//! insert and patch names). Google makes the room and puts the link in the
//! invitation it mails; the answer comes back through the sync like any
//! conference, so the card's Join button is the same one. Only where the
//! calendar allows it ([`super::StoredCalendar::offers_meet`]).

use serde_json::{Value, json};

/// Google's name for a Meet, in `conferenceSolutionKey.type` and in a
/// calendar's `allowedConferenceSolutionTypes`.
pub const MEET_TYPE: &str = "hangoutsMeet";

/// The `conferenceData` that asks Google for a new Meet. `request_id` is
/// Google's idempotency key for it: "If the request ID is the same as in a
/// previous request, the request is ignored", so a create sent again under
/// its own event id ([`request_id`]) never makes a second room.
pub fn create_request(request_id: &str) -> Value {
    json!({
        "createRequest": {
            "requestId": request_id,
            "conferenceSolutionKey": {"type": MEET_TYPE},
        }
    })
}

/// The request id for [`create_request`]: the create's own event id when it
/// has one (so a resend is the same request), else 32 random hex digits.
pub fn request_id(event_id: Option<&str>) -> String {
    if let Some(id) = event_id {
        return format!("meet-{id}");
    }
    let mut bytes = [0u8; 16];
    // A failure to read randomness is a fixed id: Google then answers a
    // second Meet request with the first room, never a wrong one.
    let _ = getrandom::fill(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The longest join link handed out.
const URL_CAP: usize = 2048;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeetingKind {
    Meet,
    Zoom,
    Teams,
    Webex,
    /// Google's conference says video, on a service not named here.
    Video,
}

impl MeetingKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Meet => "meet",
            Self::Zoom => "zoom",
            Self::Teams => "teams",
            Self::Webex => "webex",
            Self::Video => "video",
        }
    }
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "meet" => Some(Self::Meet),
            "zoom" => Some(Self::Zoom),
            "teams" => Some(Self::Teams),
            "webex" => Some(Self::Webex),
            "video" => Some(Self::Video),
            _ => None,
        }
    }
}

/// The meeting service a host runs, if it is one known to.
pub fn kind_of_host(host: &str) -> Option<MeetingKind> {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    let is = |domain: &str| host == domain || host.ends_with(&format!(".{domain}"));
    if host == "meet.google.com" {
        Some(MeetingKind::Meet)
    } else if is("zoom.us") || is("zoom.com") || is("zoomgov.com") {
        Some(MeetingKind::Zoom)
    } else if host == "teams.microsoft.com"
        || host == "teams.live.com"
        || host == "teams.microsoft.us"
    {
        Some(MeetingKind::Teams)
    } else if is("webex.com") {
        Some(MeetingKind::Webex)
    } else {
        None
    }
}

/// An `https` URL of at most [`URL_CAP`] characters, with its host.
fn https(raw: &str) -> Option<(url::Url, String)> {
    if raw.len() > URL_CAP {
        return None;
    }
    let url = url::Url::parse(raw).ok()?;
    if url.scheme() != "https" || !url.username().is_empty() || url.password().is_some() {
        return None;
    }
    let host = url.host_str()?.to_string();
    Some((url, host))
}

/// Google's video entry point, as it gave it.
fn conference_uri(item: &Value) -> Option<&str> {
    item.get("conferenceData")?
        .get("entryPoints")?
        .as_array()?
        .iter()
        .find(|e| e.get("entryPointType").and_then(Value::as_str) == Some("video"))?
        .get("uri")?
        .as_str()
}

/// Every `https://` link in free text or HTML, in order: cut at whitespace,
/// a quote, an angle bracket or a closing bracket, trailing punctuation
/// dropped, `&amp;` read as `&` (an `href` in an invitation's HTML).
fn links(text: &str) -> impl Iterator<Item = String> + '_ {
    text.match_indices("https://").map(move |(at, _)| {
        let rest = &text[at..];
        let end = rest
            .find(|c: char| c.is_whitespace() || "\"'<>)]".contains(c))
            .unwrap_or(rest.len());
        rest[..end]
            .trim_end_matches(['.', ',', ';', ':', '!', '?'])
            .replace("&amp;", "&")
    })
}

/// The first link in the location, then the description, on a meeting host.
fn text_meeting(item: &Value) -> Option<(MeetingKind, String)> {
    ["location", "description"]
        .iter()
        .filter_map(|key| item.get(*key).and_then(Value::as_str))
        .flat_map(links)
        .find_map(|link| {
            let (url, host) = https(&link)?;
            Some((kind_of_host(&host)?, url.to_string()))
        })
}

/// The meeting and its link, by the order in the module doc.
fn find(item: &Value) -> Option<(MeetingKind, String)> {
    if let Some((url, host)) = conference_uri(item).and_then(https) {
        return Some((
            kind_of_host(&host).unwrap_or(MeetingKind::Video),
            url.to_string(),
        ));
    }
    if let Some((url, host)) = item
        .get("hangoutLink")
        .and_then(Value::as_str)
        .and_then(https)
        && kind_of_host(&host) == Some(MeetingKind::Meet)
    {
        return Some((MeetingKind::Meet, url.to_string()));
    }
    text_meeting(item)
}

/// What the sync stores: which service, never the link.
pub fn meeting_kind(item: &Value) -> Option<MeetingKind> {
    find(item).map(|(kind, _)| kind)
}

/// The link Join opens, read from Google's event at the moment it is
/// pressed. Whole, passcode and all: it goes to the app and nowhere else.
pub fn join_url(item: &Value) -> Option<String> {
    find(item).map(|(_, url)| url)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn googles_conference_comes_first() {
        let item = json!({
            "conferenceData": {"entryPoints": [
                {"entryPointType": "phone", "uri": "tel:+1-555"},
                {"entryPointType": "video", "uri": "https://meet.google.com/abc-defg-hij"},
            ]},
            "location": "https://zoom.us/j/1?pwd=x",
        });
        assert_eq!(meeting_kind(&item), Some(MeetingKind::Meet));
        assert_eq!(
            join_url(&item).as_deref(),
            Some("https://meet.google.com/abc-defg-hij")
        );
        let addon = json!({"conferenceData": {"entryPoints": [
            {"entryPointType": "video", "uri": "https://us02web.zoom.us/j/8?pwd=Secret1"}]}});
        assert_eq!(meeting_kind(&addon), Some(MeetingKind::Zoom));
        assert_eq!(
            join_url(&addon).as_deref(),
            Some("https://us02web.zoom.us/j/8?pwd=Secret1"),
            "the passcode kept"
        );
        let other = json!({"conferenceData": {"entryPoints": [
            {"entryPointType": "video", "uri": "https://video.example.com/room/1"}]}});
        assert_eq!(meeting_kind(&other), Some(MeetingKind::Video));
        assert_eq!(
            meeting_kind(&json!({"hangoutLink": "https://meet.google.com/x"})),
            Some(MeetingKind::Meet)
        );
        assert_eq!(
            meeting_kind(&json!({"hangoutLink": "https://evil.example/x"})),
            None
        );
    }

    #[test]
    fn a_link_in_the_text_counts_only_on_a_meeting_host() {
        let zoom = json!({"location": "Zoom: https://zoom.us/j/123?pwd=abc."});
        assert_eq!(
            join_url(&zoom).as_deref(),
            Some("https://zoom.us/j/123?pwd=abc")
        );
        let html = json!({"description":
            "<p>Agenda</p><a href=\"https://teams.microsoft.com/l/meetup-join/19%3ameeting_X%40thread.v2/0?context=%7b%7d&amp;tid=1\">Join</a>"});
        assert_eq!(meeting_kind(&html), Some(MeetingKind::Teams));
        assert!(
            join_url(&html).unwrap().contains("&tid=1"),
            "an href's &amp; read back"
        );
        let first = json!({"description": "Slides https://docs.example.com/x then https://acme.webex.com/meet/pat"});
        assert_eq!(
            meeting_kind(&first),
            Some(MeetingKind::Webex),
            "the first MEETING link, not the first link"
        );
        for none in [
            json!({"description": "see https://docs.example.com/x"}),
            json!({"location": "http://zoom.us/j/1"}),
            json!({"location": "https://zoom.us.evil.example/j/1"}),
            json!({"location": "https://user:pw@zoom.us/j/1"}),
            json!({"location": "Room 4"}),
            json!({}),
        ] {
            assert_eq!(meeting_kind(&none), None, "{none}");
        }
        let long = json!({"location": format!("https://zoom.us/j/1?x={}", "a".repeat(3000))});
        assert_eq!(join_url(&long), None, "too long to hand out");
    }

    #[test]
    fn hosts() {
        assert_eq!(kind_of_host("Zoom.US"), Some(MeetingKind::Zoom));
        assert_eq!(kind_of_host("company.zoom.us"), Some(MeetingKind::Zoom));
        assert_eq!(kind_of_host("notzoom.us"), None);
        assert_eq!(kind_of_host("teams.live.com"), Some(MeetingKind::Teams));
        assert_eq!(kind_of_host("google.com"), None);
        for kind in [
            MeetingKind::Meet,
            MeetingKind::Zoom,
            MeetingKind::Teams,
            MeetingKind::Webex,
            MeetingKind::Video,
        ] {
            assert_eq!(MeetingKind::parse(kind.as_str()), Some(kind));
        }
    }
}
