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
    /// Wave 5. Who to put on it, bare addresses. Nil is no key.
    var guests: [String]?
    /// Wave 5. `true` has Google email those guests the invitation. Nil is
    /// no key, which the daemon reads as "tell nobody". Set in one place:
    /// `stamped(notify:)`, from `EventGuests.notify`.
    var notify: Bool?

    /// The body with what a labelled control decided about telling people.
    /// True is written only onto a body that names somebody.
    func stamped(notify: Bool) -> EventWrite {
        var out = self
        out.notify = notify && !(guests ?? []).isEmpty ? true : nil
        return out
    }
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
    /// Wave 5. The WHOLE new guest list (it replaces the event's), or nil to
    /// leave the guests alone. An empty list takes everyone off.
    var guests: [String]?

    var isEmpty: Bool { self == EventPatch() }

    /// The body: changed keys only, and NSNull for a cleared text. `notify`
    /// is not a field of the patch (a patch that changes nothing must stay
    /// empty): it is said here, by the caller that holds a labelled
    /// control's answer, and written only when it is true.
    func json(notify: Bool = false) -> [String: Any] {
        var out: [String: Any] = [:]
        if let guests { out["guests"] = guests }
        if notify { out["notify"] = true }
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

/// WHOSE ANSWER IT IS. Taken when a write (a save, a delete, a ghost's mark)
/// is sent, and asked again when its answer lands: the account it was sent
/// for (`epoch`) and the calendar session it was sent in (`session`, bumped
/// by `AppStore.dropCalendarState`, which an account switch AND the gate
/// closing both run). An answer whose ticket is not the live one is dropped
/// whole: nothing laid into the feed, no toast, no analytics.
struct CalendarWriteTicket: Hashable, Sendable {
    let epoch: Int
    let session: Int

    /// The answer may be applied: nothing has been dropped since it was sent.
    func lands(live: CalendarWriteTicket) -> Bool { self == live }
}

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

    /// A delete's query: `notify=true` when a labelled control said to tell
    /// the guests, and nothing at all otherwise.
    static func deleteQuery(notify: Bool) -> [String: String?] {
        notify ? ["notify": "true"] : [:]
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

    // MARK: an answer that never came (504)

    /// The daemon's word for "Google was asked and the answer was lost": a
    /// timeout or a dropped connection after the request went out. Google
    /// may well have made the change (and emailed the guests), so it is NOT
    /// a refusal and the same control must not simply be pressed again.
    static let uncertainStatus = 504

    /// Said at once, while the page reloads to find out.
    static let uncertainText = "Google may have made this change. Refreshing…"

    /// Said when the reloads still do not show the change. The pane stays
    /// open with everything in it.
    static let uncertainUnseenText = "Not on your calendar yet. Check it before trying again."

    /// An event's identity in a feed.
    static func key(_ e: TimelineEvent) -> String { e.calendar_id + "\n" + e.id }

    /// Every event the feed holds, by identity: taken before a create, so
    /// the event it made can be told from one already there.
    static func keys(_ feed: TimelineFeed?) -> Set<String> { Set((feed?.events ?? []).map(key)) }

    /// After an uncertain create: the event the reloaded feed shows that the
    /// write would have made. On the calendar written to, with the title
    /// sent, and not one of the events there before it was sent.
    static func made(_ body: EventWrite, before: Set<String>, in feed: TimelineFeed?) -> TimelineEvent? {
        (feed?.events ?? []).first {
            $0.calendar_id == body.calendar_id && ($0.title ?? "") == body.title && !before.contains(key($0))
        }
    }

    /// After an uncertain edit: the event as the reloaded feed has it, if it
    /// is no longer the event the pane opened on. Nil while it still is (or
    /// it is gone, which is not this edit's doing).
    static func edited(_ original: TimelineEvent, in feed: TimelineFeed?) -> TimelineEvent? {
        guard let now = (feed?.events ?? []).first(where: { key($0) == key(original) }), now != original
        else { return nil }
        return now
    }

    /// After an uncertain delete: the reloaded feed no longer has it. A feed
    /// that never loaded proves nothing.
    static func gone(_ event: TimelineEvent, from feed: TimelineFeed?) -> Bool {
        guard let events = feed?.events else { return false }
        return !events.contains { key($0) == key(event) }
    }

    /// A refusal, as one quiet toast. Fixed sentences: the daemon's own error
    /// text is for logs, and none of this echoes what was typed. `status` 0
    /// is a request that never reached the daemon.
    ///
    /// `guests` says the write named guests: the daemon's 400 then may be
    /// about one of them, and the sentence says where to look.
    static func failureText(status: Int, _ action: CalendarWriteAction, guests: Bool = false) -> String {
        let deleting = action == .delete
        switch status {
        case 0:
            return deleting
                ? "Could not reach your Passband server, so the event was not deleted."
                : "Could not reach your Passband server, so nothing was saved."
        case 400:
            return deleting
                ? "Passband could not delete that event."
                : (guests
                    ? "Passband could not save that event. Check the times, the calendar and the guests."
                    : "Passband could not save that event. Check the times and the calendar.")
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
        case uncertainStatus:
            return uncertainText
        default:
            return deleting ? "Could not delete the event." : "Could not save the event."
        }
    }
}
