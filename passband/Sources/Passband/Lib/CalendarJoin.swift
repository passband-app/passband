// JOIN: an event's online meeting, one press from its card (docs/CALENDAR.md,
// "Join"). The feed says only which service (`TimelineEvent.meeting`); the
// link itself, passcode and all, is asked of the daemon when Join is pressed
// (`GET /client/calendar/events/{calendar_id}/{id}/join`), opened, and kept
// nowhere.
//
// PURE, Foundation only (Tests/CalendarReplyTests.swift).

import Foundation

enum CalendarJoin {
    /// The button's words, by the daemon's service word. Nil for no meeting
    /// (or a word this app does not know, from a newer daemon: still "Join").
    static func label(_ meeting: String?) -> String? {
        guard let meeting, !meeting.isEmpty else { return nil }
        switch meeting {
        case "zoom": return "Join Zoom"
        case "meet": return "Join Google Meet"
        case "teams": return "Join Teams"
        case "webex": return "Join Webex"
        default: return "Join"
        }
    }

    /// The route for one event.
    static func path(calendarId: String, eventId: String) -> String {
        CalendarWrite.path(calendarId: calendarId, eventId: eventId) + "/join"
    }

    /// Said when the link could not be had.
    static let failedText = "Could not get the meeting link from Google Calendar."
    static let goneText = "This event no longer has a meeting link."
}
