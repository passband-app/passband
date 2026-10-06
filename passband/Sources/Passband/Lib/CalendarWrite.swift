// AROUND A WRITE TO A REAL CALENDAR: the bodies the daemon is sent, which
// calendars the editor offers, the route's path, how the daemon's answer is
// laid into the feed on screen, and the sentence a refusal becomes.
//
// PURE, and Foundation only, with nothing but the wire types under it: the
// API client compiles against this file alone in test.sh's suites. What the
// user typed becomes one of these bodies in Lib/EventDraft.swift.

import Foundation

/// `POST /client/calendar/events`. Nil optionals are dropped by the shared
/// encoder, which is what "absent" means here.
struct EventWrite: Encodable, Hashable, Sendable {
    let calendar_id: String
    let title: String
    let start: String
    let end: String
    let all_day: Bool
    let location: String?
    let description: String?
}

/// `PATCH /client/calendar/events/{calendar_id}/{id}`: only what changed.
/// `location` and `description` take an explicit null to clear, which the
/// shared encoder cannot say, so the body is built from `json`.
struct EventPatch: Hashable, Sendable {
    enum Text: Hashable, Sendable {
        case set(String)
        case clear
    }

    var title: String?
    var start: String?
    var end: String?
    var all_day: Bool?
    var location: Text?
    var description: Text?

    var isEmpty: Bool { self == EventPatch() }

    /// The body: changed keys only, and NSNull for a cleared text.
    func json() -> [String: Any] {
        var out: [String: Any] = [:]
        if let title { out["title"] = title }
        if let start { out["start"] = start }
        if let end { out["end"] = end }
        if let all_day { out["all_day"] = all_day }
        func text(_ t: Text) -> Any {
            switch t {
            case .set(let s): s
            case .clear: NSNull()
            }
        }
        if let location { out["location"] = text(location) }
        if let description { out["description"] = text(description) }
        return out
    }
}

// MARK: - around a write

/// What a write is, for the sentence its refusal becomes.
enum CalendarWriteAction: Sendable {
    case create
    case edit
    case delete
}

enum CalendarWrite {
    /// The calendars the editor offers: writable ones, primary first, then in
    /// the daemon's order. Empty on an older daemon (no `calendars`), and
    /// then nothing offers to create an event.
    static func writable(_ feed: TimelineFeed?) -> [TimelineCalendar] {
        let w = (feed?.calendars ?? []).filter { $0.writable == true }
        return w.filter { $0.primary == true } + w.filter { $0.primary != true }
    }

    static func canCreate(_ feed: TimelineFeed?) -> Bool { !writable(feed).isEmpty }

    /// An event the user may change: the daemon said so in as many words.
    /// Nil (an older daemon) is no.
    static func canEdit(_ event: TimelineEvent?) -> Bool { event?.editable == true }

    /// A calendar's name in the picker.
    static func name(_ c: TimelineCalendar) -> String {
        let n = (c.name ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
        if !n.isEmpty { return n }
        return c.primary == true ? "Your calendar" : "Calendar"
    }

    /// The route for one event. Ids are Google's and can carry `#`, `@` or
    /// `/` (a holiday calendar's id has two of them), so each is
    /// percent-encoded as a single path segment.
    static func path(calendarId: String, eventId: String) -> String {
        "/client/calendar/events/\(segment(calendarId))/\(segment(eventId))"
    }

    static func segment(_ s: String) -> String {
        s.addingPercentEncoding(withAllowedCharacters: segmentAllowed) ?? s
    }

    private static let segmentAllowed = CharacterSet(
        charactersIn: "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~@")

    /// The daemon's answer laid into the feed on screen: a new event added, an
    /// edited one replaced in place. A feed from an older shape with no
    /// `events` gains the list.
    static func applying(_ event: TimelineEvent, to feed: TimelineFeed?) -> TimelineFeed {
        var f = feed ?? TimelineFeed()
        var events = f.events ?? []
        if let i = events.firstIndex(where: { $0.calendar_id == event.calendar_id && $0.id == event.id }) {
            events[i] = event
        } else {
            events.append(event)
        }
        f.events = events
        return f
    }

    static func removing(calendarId: String, id: String, from feed: TimelineFeed?) -> TimelineFeed? {
        guard var f = feed else { return nil }
        f.events = f.events?.filter { !($0.calendar_id == calendarId && $0.id == id) }
        return f
    }

    /// A refusal, as one quiet toast. Fixed sentences: the daemon's own error
    /// text is for logs, and none of this echoes what was typed. `status` 0
    /// is a request that never reached the daemon.
    static func failureText(status: Int, _ action: CalendarWriteAction) -> String {
        let deleting = action == .delete
        switch status {
        case 0:
            return deleting
                ? "Could not reach your Passband server, so the event was not deleted."
                : "Could not reach your Passband server, so nothing was saved."
        case 400:
            return deleting
                ? "Passband could not delete that event."
                : "Passband could not save that event. Check the times and the calendar."
        case 403:
            return "That calendar does not let you change its events."
        case 404:
            return action == .create
                ? "This Passband server cannot create events yet."
                : "That event is no longer on your calendar."
        case 502:
            return deleting
                ? "Google Calendar refused, so the event was not deleted."
                : "Google Calendar refused the change, so nothing was saved."
        default:
            return deleting ? "Could not delete the event." : "Could not save the event."
        }
    }
}
