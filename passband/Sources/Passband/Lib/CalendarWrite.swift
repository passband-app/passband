// AROUND A WRITE TO A REAL CALENDAR: the bodies the daemon is sent, which
// calendars the editor offers, the route's path, how the daemon's answer is
// laid into the feed on screen, and the sentence a refusal becomes.
//
// PURE, and Foundation only, with nothing but the wire types and APIError
// under it: the API client compiles against this file alone in test.sh's
// suites. What the
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
    /// The new event's id, chosen here (`CalendarWrite.newEventId`), one per
    /// pane and the same on every press of it. The daemon creates the event
    /// under it, and answers a second send with the event the first one made,
    /// so a create retried after an uncertain answer can never make two or
    /// invite twice. Nil is no key: Google picks the id.
    var id: String?

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

    /// A new event's id, as Google takes a client's: "lowercase letters a-v
    /// and digits 0-9", 5 to 1024 of them. A UUID's 32 hex digits are both,
    /// and Google recommends "an established UUID algorithm" against
    /// collisions.
    static func newEventId() -> String {
        UUID().uuidString.lowercased().replacingOccurrences(of: "-", with: "")
    }

    /// The status a failed write is judged by: its HTTP status, or 0 for a
    /// request that never reached the daemon, or -1 for anything that is
    /// not a status at all (an answer of the wrong shape). A wait that ran
    /// out, or a connection lost mid-request, may have reached the daemon,
    /// which goes on with the write without the app: that is the 504 "may
    /// have happened", settled against the page, never "nothing was saved".
    static func status(of error: Error) -> Int {
        guard let api = error as? APIError else { return -1 }
        if api.kind == .network { return api.mayHaveArrived ? uncertainStatus : 0 }
        return api.status == 0 ? -1 : api.status
    }

    /// What a delete tells the guests, from the question's answer and the
    /// page as it is when the delete runs. Nil REFUSES the delete: "delete
    /// and tell guests" was pressed, there are guests, and the calendar may
    /// no longer tell them (the page reloaded under the question). Deleting
    /// silently then would not be what the control said, so nothing is sent,
    /// as a save refuses a control that is no longer on offer.
    static func deleteNotify(tellGuests: Bool, hasGuests: Bool, mayTell: Bool) -> Bool? {
        guard tellGuests, hasGuests else { return false }
        return mayTell ? true : nil
    }

    /// Said when `deleteNotify` refuses.
    static let cannotTellText = "This calendar can no longer tell guests, so the event was not deleted."

    // MARK: text as the daemon stores it

    /// Text the way the daemon stores what Google holds, as near as the
    /// app can say it: control, bidi and invisible characters out (the
    /// daemon's `plain`), whitespace runs collapsed, trimmed. Used to
    /// recognise what was typed in what the reloaded page shows.
    static func laundered(_ s: String) -> String {
        let invisible: Set<UInt32> = [0x061C, 0x200B, 0x200C, 0x200D, 0x2060, 0xFEFF, 0x200E, 0x200F]
        let kept = s.unicodeScalars.filter { u in
            if CharacterSet.whitespacesAndNewlines.contains(u) { return true }
            if CharacterSet.controlCharacters.contains(u) { return false }
            let v = u.value
            if invisible.contains(v) || (0x202A...0x202E).contains(v) || (0x2066...0x2069).contains(v)
                || (0xE0000...0xE007F).contains(v)
            {
                return false
            }
            return true
        }
        return String(String.UnicodeScalarView(kept))
            .components(separatedBy: .whitespacesAndNewlines).filter { !$0.isEmpty }.joined(separator: " ")
    }

    /// `laundered`, and every link cut to host and path, without `www.`,
    /// a query or a fragment: how a location or a note comes back.
    static func looseText(_ s: String?) -> String {
        laundered(s ?? "").split(separator: " ").map { word -> String in
            let w = String(word)
            guard w.contains("://") || w.lowercased().hasPrefix("www."),
                let url = URL(string: w.contains("://") ? w : "https://" + w), var host = url.host
            else { return w }
            host = host.lowercased()
            if host.hasPrefix("www.") { host.removeFirst(4) }
            var path = url.path
            while path.hasSuffix("/") { path.removeLast() }
            return host + path
        }.joined(separator: " ")
    }

    /// Two wire times name the same moment (a timed one) or day (all-day).
    static func sameTime(_ a: String?, _ b: String?) -> Bool {
        guard let a, let b else { return a == b }
        if a == b { return true }
        let f = ISO8601DateFormatter()
        guard let x = f.date(from: a), let y = f.date(from: b) else { return false }
        return x == y
    }

    /// An event's identity in a feed.
    static func key(_ e: TimelineEvent) -> String { e.calendar_id + "\n" + e.id }

    /// Every event the feed holds, by identity: taken before a create, so
    /// the event it made can be told from one already there.
    static func keys(_ feed: TimelineFeed?) -> Set<String> { Set((feed?.events ?? []).map(key)) }

    /// After an uncertain create: the event the reloaded feed shows that the
    /// write made. With an id (the app always sends one): THE event with
    /// that id on the calendar written to, whatever its title came back as.
    /// Without one (never sent by this app; kept for a body built by hand):
    /// a new event on that calendar at that start whose title, laundered as
    /// the daemon launders it, is the one sent.
    static func made(_ body: EventWrite, before: Set<String>, in feed: TimelineFeed?) -> TimelineEvent? {
        let events = feed?.events ?? []
        if let id = body.id {
            return events.first { $0.calendar_id == body.calendar_id && $0.id == id }
        }
        return events.first {
            $0.calendar_id == body.calendar_id && !before.contains(key($0))
                && laundered($0.title ?? "") == laundered(body.title) && sameTime($0.start, body.start)
        }
    }

    /// After an uncertain edit: the event as the reloaded feed has it, once
    /// EVERY field the edit changed reads as the user typed it (laundered as
    /// the daemon launders it). Anything the edit did not touch (a guest's
    /// answer, the colour) is not looked at, so a change somebody else made
    /// meanwhile is never taken for this one. Nil while any changed field
    /// still differs, or the event is gone (which is not this edit's doing).
    static func edited(_ original: TimelineEvent, _ patch: EventPatch, in feed: TimelineFeed?) -> TimelineEvent? {
        guard let now = (feed?.events ?? []).first(where: { key($0) == key(original) }) else { return nil }
        if let title = patch.title, laundered(now.title ?? "") != laundered(title) { return nil }
        if let allDay = patch.all_day, now.all_day != allDay { return nil }
        if let start = patch.start, !sameTime(now.start, start) { return nil }
        if let end = patch.end {
            // A one-day all-day event is served with no end: its last day is
            // its start.
            let served = now.end ?? (now.all_day ? now.start : nil)
            if !sameTime(served, end) { return nil }
        }
        for (typed, held) in [(patch.location, now.location), (patch.description, now.description)] {
            switch typed {
            case nil: continue
            case .clear?: if !looseText(held).isEmpty { return nil }
            case .set(let text)?: if looseText(held) != looseText(text) { return nil }
            }
        }
        if let guests = patch.guests {
            let held = Set(now.guests.map { $0.filter { $0.isSelf != true }.map { $0.email.lowercased() } } ?? [])
            if held != Set(guests.map { $0.lowercased() }) { return nil }
        }
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
        case 409:
            return action == .create
                ? "That event was already added and then deleted. Close this and start a new one."
                : (deleting ? "Could not delete the event." : "Could not save the event.")
        case 410:
            return deleting
                ? "That calendar is no longer available to you, so nothing was deleted."
                : "That calendar is no longer available to you, so nothing was saved."
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
