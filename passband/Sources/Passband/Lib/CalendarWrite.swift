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
    /// pane (`CalendarWrite.createWrite`). The daemon reads it before
    /// inserting, and answers a second send of the SAME event (same calendar,
    /// title, times and guests) with the one the first send made; the same id
    /// with anything else is refused (409). After an uncertain answer the
    /// pane freezes (`EventPaneState`) and can only send this exact body
    /// again, so a retry cannot make a second event on that calendar or
    /// invite twice. Google "cannot guarantee" to catch a taken id at insert
    /// time, and a read can lag, so this narrows the window rather than
    /// closing it outright (docs/CALENDAR.md section 8). Nil is no key: Google
    /// picks the id.
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

    // MARK: an answer that may not be the whole story

    /// Said at once, while the page reloads to find out.
    static let uncertainText = "Google may have made this change. Refreshing…"

    /// Said when the reloads still do not show a DELETE. The question stays
    /// up. (A save that cannot be confirmed freezes its pane instead, which
    /// says `frozenText`.)
    static let uncertainUnseenText = "Not on your calendar yet. Check it before trying again."

    /// The frozen pane's one line (`EventPaneState.uncertain`).
    static let frozenText = "Google may have made this change. Passband could not confirm it yet."

    /// The same line while "check again" or "send again" is out.
    static let frozenCheckingText = "Checking with Google Calendar…"

    /// Said when "send again" is refused outright: the first send may still
    /// have worked, so the pane stays frozen.
    static let resendRefusedText = "Sending again did not go through. The first send may still have worked."

    /// A new event's id, as Google takes a client's: "lowercase letters a-v
    /// and digits 0-9", 5 to 1024 of them. A UUID's 32 hex digits are both,
    /// and Google recommends "an established UUID algorithm" against
    /// collisions.
    static func newEventId() -> String {
        UUID().uuidString.lowercased().replacingOccurrences(of: "-", with: "")
    }

    /// Said when the daemon refused a create because the same one is still
    /// being sent (`already_creating`): the pane holds, since that send may
    /// yet make the event.
    static let stillSendingText = "This event is still being sent. Check again in a moment."

    /// WHAT A FAILED WRITE SAYS HAPPENED AT GOOGLE (verify4 X1). Unlocking
    /// the pane is allowed ONLY on a definite "nothing happened": the
    /// daemon's own refusal before any Google call, or Google's own
    /// refusal, each named by its `code` from the closed list
    /// (`CalendarRefusal`) AND carried with that code's own status. A
    /// request that never left the app is the other definite case.
    /// Everything else may have happened: a 504, any 500, a 502 without
    /// `google_refused` (a proxy's own page), an unknown code, no code, a
    /// 2xx that would not decode, a transport failure after the request may
    /// have left, and `already_creating` (another send of the same create is
    /// still out and may make it). When in doubt, it may have happened.
    static func verdict(of error: Error) -> CalendarWriteVerdict {
        guard let api = error as? APIError else { return .mayHaveHappened(code: nil) }
        if api.kind == .network { return api.mayHaveArrived ? .mayHaveHappened(code: nil) : .notSent }
        guard let code = api.code, let refusal = CalendarRefusal(rawValue: code), refusal.status == api.status
        else { return .mayHaveHappened(code: api.code) }
        return .refused(refusal)
    }

    /// The toast while a write that may have happened is settled against
    /// the page.
    static func settlingText(_ verdict: CalendarWriteVerdict) -> String {
        verdict == .mayHaveHappened(code: CalendarRefusal.alreadyCreatingCode) ? stillSendingText : uncertainText
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

    // MARK: the bodies a pane sends

    /// A create's body: the wire draft, what the control decided about
    /// telling the guests (`EventWrite.stamped`), and THE PANE'S id. Every
    /// send from one pane carries the same `createId`; a new pane makes a new
    /// one (`newEventId`).
    static func createWrite(_ made: EventWrite, notify: Bool, createId: String, before: Set<String>) -> SentWrite {
        var body = made.stamped(notify: notify)
        body.id = createId
        return .create(body, before: before)
    }

    /// An edit's body, or nil when it changes nothing (nothing is sent, and
    /// nobody is told whatever the control said).
    static func editWrite(_ original: TimelineEvent, _ patch: EventPatch, notify: Bool) -> SentWrite? {
        patch.isEmpty ? nil : .edit(original, patch, notify: notify)
    }

    /// A delete: the event and whether Google tells its guests, or nil when
    /// `deleteNotify` refuses it (nothing is sent).
    static func deleteWrite(
        _ event: TimelineEvent, tellGuests: Bool, hasGuests: Bool, mayTell: Bool
    ) -> (calendarId: String, id: String, notify: Bool)? {
        guard let notify = deleteNotify(tellGuests: tellGuests, hasGuests: hasGuests, mayTell: mayTell)
        else { return nil }
        return (event.calendar_id, event.id, notify)
    }

    // MARK: text as the daemon stores it

    /// Text the way the daemon stores what Google holds, as near as the
    /// app can say it: control, bidi and invisible characters out (the
    /// daemon's `plain`), whitespace runs collapsed, trimmed. Used only to
    /// recognise a create's title when no event carries its id (`made`),
    /// and to read an uncertain edit's text fields back (`edited`).
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
    /// write made. First THE event with the body's id on the calendar written
    /// to, whatever its title came back as. When no event carries the id (no
    /// id was sent, or a calendar daemon older than ids ignored it and let
    /// Google pick one), the fallback (verify4 X3) takes only an event that
    /// could be nobody else's: on that calendar, not in the page before the
    /// send, the user's own (`editable`, which the daemon gives only an
    /// event the user organizes on a calendar they write to, and no guest
    /// named as another organizer), the same all-day flag, start AND end,
    /// and the title, laundered as the daemon launders it. Someone else's
    /// invitation with the same title at the same time is not it.
    static func made(_ body: EventWrite, before: Set<String>, in feed: TimelineFeed?) -> TimelineEvent? {
        let events = feed?.events ?? []
        if let id = body.id, let byId = events.first(where: { $0.calendar_id == body.calendar_id && $0.id == id }) {
            return byId
        }
        return events.first {
            $0.calendar_id == body.calendar_id && !before.contains(key($0)) && ownedByUser($0)
                && $0.all_day == body.all_day
                && laundered($0.title ?? "") == laundered(body.title) && sameTime($0.start, body.start)
                && sameTime(servedEnd($0), body.end)
        }
    }

    /// The user organizes it, as the daemon judges (`editable`), and no
    /// guest is named as another organizer.
    static func ownedByUser(_ e: TimelineEvent) -> Bool {
        e.editable == true && !(e.guests ?? []).contains { $0.organizer == true && $0.isSelf != true }
    }

    /// An event's end as a body names it: a one-day all-day event is served
    /// with no end, its last day being its start.
    static func servedEnd(_ e: TimelineEvent) -> String? { e.end ?? (e.all_day ? e.start : nil) }

    /// After an uncertain edit: the event as the reloaded feed has it, once
    /// EVERY field the edit changed reads there AS SENT. Times, the all-day
    /// flag and the guests come back exactly. A text field (title, place,
    /// notes) counts only when the served value equals what was typed after
    /// `laundered` (whitespace collapsed, invisible characters out): a field
    /// that MOVED to something else is not this edit (verify4 X2: someone
    /// else's change, or this user's own earlier edit, landing meanwhile)
    /// and leaves the answer unknown. So does text the daemon rewrites in
    /// ways the app does not copy (a link cut down, markup out, the cap):
    /// the pane stays frozen and is closed by hand, which is safe. Anything
    /// the edit did not touch (a guest's answer, the colour) is not looked
    /// at.
    ///
    /// Nil is "not confirmed": while any changed field does not read as
    /// sent, while nothing the edit changed can be observed at all, or when
    /// the event is gone. With the pane frozen (`EventPaneState`) that
    /// answer is always safe.
    static func edited(_ original: TimelineEvent, _ patch: EventPatch, in feed: TimelineFeed?) -> TimelineEvent? {
        guard let now = (feed?.events ?? []).first(where: { key($0) == key(original) }) else { return nil }
        var seen = 0
        if let title = patch.title, laundered(title) != laundered(original.title ?? "") {
            guard laundered(now.title ?? "") == laundered(title) else { return nil }
            seen += 1
        }
        if let allDay = patch.all_day, allDay != original.all_day {
            guard now.all_day == allDay else { return nil }
            seen += 1
        }
        if let start = patch.start, !sameTime(start, original.start) {
            guard sameTime(now.start, start) else { return nil }
            seen += 1
        }
        if let end = patch.end {
            if !sameTime(end, servedEnd(original)) {
                guard sameTime(servedEnd(now), end) else { return nil }
                seen += 1
            }
        }
        for (typed, held, was) in [
            (patch.location, now.location, original.location),
            (patch.description, now.description, original.description),
        ] {
            let had = !(was ?? "").isEmpty
            switch typed {
            case nil: continue
            case .clear?:
                guard had else { continue }
                guard (held ?? "").isEmpty else { return nil }
            case .set(let text)?:
                guard laundered(text) != laundered(was ?? "") else { continue }
                guard laundered(held ?? "") == laundered(text) else { return nil }
            }
            seen += 1
        }
        if let guests = patch.guests {
            let others = { (e: TimelineEvent) in
                Set(e.guests.map { $0.filter { $0.isSelf != true }.map { $0.email.lowercased() } } ?? [])
            }
            let typed = Set(guests.map { $0.lowercased() })
            if typed != others(original) {
                guard others(now) == typed else { return nil }
                seen += 1
            }
        }
        return seen > 0 ? now : nil
    }

    /// After an uncertain delete: the reloaded feed no longer has it. A feed
    /// that never loaded proves nothing.
    static func gone(_ event: TimelineEvent, from feed: TimelineFeed?) -> Bool {
        guard let events = feed?.events else { return false }
        return !events.contains { key($0) == key(event) }
    }

    /// A definite refusal, as one quiet toast. Fixed sentences, one per
    /// code: the daemon's own error text is for logs, and none of this
    /// echoes what was typed. Nil is a request that never left the app.
    ///
    /// `guests` says the write named guests: the daemon's `invalid` then may
    /// be about one of them, and the sentence says where to look.
    static func failureText(_ refusal: CalendarRefusal?, _ action: CalendarWriteAction, guests: Bool = false) -> String {
        let deleting = action == .delete
        let nothing = deleting ? "so the event was not deleted." : "so nothing was saved."
        switch refusal {
        case nil:
            return "Could not reach your Passband server, \(nothing)"
        case .invalid:
            return deleting
                ? "Passband could not delete that event."
                : (guests
                    ? "Passband could not save that event. Check the times, the calendar and the guests."
                    : "Passband could not save that event. Check the times and the calendar.")
        case .unknownCalendar:
            return "Passband does not read that calendar, \(nothing)"
        case .readOnly:
            return "That calendar is read-only, \(nothing)"
        case .cannotInvite:
            return "Guests can only be added on your own calendar, so nothing was saved."
        case .guestsReadOnly:
            return "This event's guests cannot be changed from Passband, so nothing was saved."
        case .cannotNotify:
            return "Guests can only be told about changes on your own calendar, \(nothing)"
        case .notGranted:
            return "Google Calendar is not connected, \(nothing)"
        case .notEditable:
            return "That calendar does not let you change its events."
        case .notFound:
            return "That event is no longer on your calendar."
        case .idInUse:
            return "Passband could not save this event under its id. Close this and start a new one."
        case .alreadyDeleted:
            return "That event was already added and then deleted. Close this and start a new one."
        case .calendarGone:
            return "That calendar is no longer available to you, \(deleting ? "so nothing was deleted." : nothing)"
        case .googleRefused:
            return deleting
                ? "Google Calendar refused, so the event was not deleted."
                : "Google Calendar refused the change, so nothing was saved."
        }
    }
}

/// THE DAEMON'S CLOSED LIST of definite refusals (docs/CALENDAR.md section
/// 8, "What a refusal says"): each is a write nothing of which reached
/// Google, or one Google itself refused, and each comes with its own
/// status. A code not on this list, or this list's code with another
/// status, is not a refusal (`CalendarWrite.verdict`).
enum CalendarRefusal: String, CaseIterable, Hashable, Sendable {
    case invalid
    case unknownCalendar = "unknown_calendar"
    case readOnly = "read_only"
    case cannotInvite = "cannot_invite"
    case guestsReadOnly = "guests_read_only"
    case cannotNotify = "cannot_notify"
    case notGranted = "not_granted"
    case notEditable = "not_editable"
    case notFound = "not_found"
    case idInUse = "id_in_use"
    case alreadyDeleted = "already_deleted"
    case calendarGone = "calendar_gone"
    case googleRefused = "google_refused"

    /// The status the daemon sends this code with.
    var status: Int {
        switch self {
        case .invalid, .unknownCalendar, .readOnly, .cannotInvite, .guestsReadOnly, .cannotNotify: 400
        case .notGranted, .notEditable: 403
        case .notFound: 404
        case .idInUse, .alreadyDeleted: 409
        case .calendarGone: 410
        case .googleRefused: 502
        }
    }

    /// The daemon's other codes, which are NOT refusals: Google may have
    /// made the change. Named here only so the tests can say so.
    static let alreadyCreatingCode = "already_creating"
    static let uncertainCode = "uncertain"
    static let internalCode = "internal"
}

/// `CalendarWrite.verdict`: what a failed write says about Google.
enum CalendarWriteVerdict: Hashable, Sendable {
    /// The request never left the app.
    case notSent
    /// A definite refusal: nothing of this send reached Google, or Google
    /// refused it.
    case refused(CalendarRefusal)
    /// Google may have made the change. `code`: the daemon's, when it named
    /// one (an unknown code included).
    case mayHaveHappened(code: String?)

    /// The pane holds and the page is checked; never "nothing was saved".
    var mayHaveHappened: Bool {
        if case .mayHaveHappened = self { return true }
        return false
    }
}

/// What a pane sent, kept whole while its answer is in doubt: "send again"
/// sends exactly this, and "check again" looks for exactly this.
enum SentWrite: Hashable, Sendable {
    /// `before`: every event the page held when it was first sent, so the
    /// event it made can be told from one already there (`made`).
    case create(EventWrite, before: Set<String>)
    case edit(TimelineEvent, EventPatch, notify: Bool)

    var isEdit: Bool {
        if case .edit = self { return true }
        return false
    }

    /// Google is asked to mail the guests.
    var tellsGuests: Bool {
        switch self {
        case .create(let body, _): body.notify == true
        case .edit(_, _, let notify): notify
        }
    }

    /// The event the reloaded page shows this write made, or nil while it
    /// cannot be confirmed.
    func found(in feed: TimelineFeed?) -> TimelineEvent? {
        switch self {
        case .create(let body, let before): CalendarWrite.made(body, before: before, in: feed)
        case .edit(let original, let patch, _): CalendarWrite.edited(original, patch, in: feed)
        }
    }
}

/// THE EVENT PANE'S WRITE, as a small state machine (fix round 3). A save
/// whose answer never came and that the reloaded page cannot confirm
/// FREEZES the pane: its fields go read-only with the draft still showing,
/// one line says Google may have made the change, and the only ways on are
/// "check again" (the page reloaded and looked at again), "send again"
/// (exactly the body already sent, `SentWrite`) and "close". Nothing leads
/// from a frozen pane back to an editable one: changing the event after an
/// uncertain save means closing and starting again, under a new id.
enum EventPaneState: Hashable, Sendable {
    /// The fields are the user's.
    case editing
    /// A write is out. `again`: sent from the frozen pane (a check or a
    /// resend), so whatever it answers, the pane never thaws.
    case sending(again: Bool)
    /// Google may have made the change; the page could not confirm it.
    case uncertain
    /// The change is on the page. The pane closes.
    case confirmed

    enum Event: Hashable, Sendable {
        /// A save control pressed in an editable pane.
        case send
        /// The daemon answered with the event, or the page showed it.
        case answered
        /// A definite refusal: Google was not changed by THIS send.
        case refused
        /// No answer, and the page does not show the change.
        case unsure
        case checkAgain
        case sendAgain
    }

    func next(_ event: Event) -> EventPaneState {
        switch (self, event) {
        case (.editing, .send): .sending(again: false)
        case (.sending, .answered): .confirmed
        case (.sending(again: false), .refused): .editing
        case (.sending(again: true), .refused): .uncertain
        case (.sending, .unsure): .uncertain
        case (.uncertain, .checkAgain), (.uncertain, .sendAgain): .sending(again: true)
        default: self
        }
    }

    /// The fields take typing.
    var fieldsEditable: Bool { self == .editing }

    /// The frozen footer is drawn: its line and its three controls.
    var frozen: Bool { self == .uncertain || self == .sending(again: true) }

    enum Control: Hashable, Sendable {
        case checkAgain, sendAgain, close
    }

    /// The frozen footer's controls that may be pressed now: all three while
    /// uncertain, only close while a check or a resend is out, none
    /// otherwise (the editable footer is the save controls').
    var frozenControls: [Control] {
        switch self {
        case .uncertain: [.checkAgain, .sendAgain, .close]
        case .sending(again: true): [.close]
        default: []
        }
    }

    enum Key: Hashable, Sendable {
        case enter, commandEnter, escape
    }

    enum KeyAction: Hashable, Sendable {
        /// The editable pane's own rules (`EventGuests.enter`, the footer's
        /// main control).
        case editingRules
        case checkAgain
        case close
        /// Swallowed: nothing happens.
        case nothing
    }

    func action(_ key: Key) -> KeyAction {
        switch (self, key) {
        case (_, .escape): .close
        case (.editing, _): .editingRules
        case (.uncertain, .commandEnter): .checkAgain
        default: .nothing
        }
    }

    static let checkAgainLabel = "check again"
    static let closeLabel = "close"

    /// "send again". An edit that tells guests says it may tell them twice;
    /// a create's id keeps Google from inviting twice, so it says nothing
    /// more.
    static func sendAgainLabel(_ sent: SentWrite) -> String {
        sent.isEdit && sent.tellsGuests ? "send again, guests may be told twice" : "send again"
    }
}

/// THE STORE'S DECISIONS ABOUT THE PANE, pure (verify4 X5): what may be
/// sent, what an answer may move, and what is said. `CalendarStore` asks
/// these and does only the I/O.
enum EventPaneRules {
    /// The pane as the store holds it when a control is pressed.
    struct Pane: Hashable, Sendable {
        /// The pane's request is the open one (`calendarEditor?.id`).
        var isOpen: Bool
        /// It is the account on screen (`request.epoch == epoch`).
        var sameAccount: Bool
        /// A write is already out (`calendarWriting`).
        var writing: Bool
        var state: EventPaneState
    }

    /// `saveEvent` may run: the open pane, this account, nothing out, and
    /// fields that are the user's. Never from a frozen pane.
    static func maySave(_ pane: Pane) -> Bool {
        pane.isOpen && pane.sameAccount && !pane.writing && pane.state.fieldsEditable
    }

    /// What "send again" and "check again" work from: EXACTLY what was sent
    /// first, and only from a frozen pane with nothing out. Nil: nothing is
    /// sent, nothing is checked. Never the draft.
    static func again(_ pane: Pane, sent: SentWrite?) -> SentWrite? {
        guard pane.isOpen, pane.sameAccount, !pane.writing, pane.state == .uncertain else { return nil }
        return sent
    }

    /// An answer may move the pane only while the pane that sent it is
    /// still the open one: a pane closed (or replaced) while its write was
    /// out keeps no state from it.
    static func applies(open: UUID?, sentFrom request: UUID) -> Bool { open == request }

    /// After a failed send: the pane's event, or nil when the write may have
    /// happened and is settled against the page first (it ends `.unsure` or
    /// `.answered`, never `.refused`).
    static func failureEvent(_ verdict: CalendarWriteVerdict) -> EventPaneState.Event? {
        verdict.mayHaveHappened ? nil : .refused
    }

    /// The toast for a failed send that did NOT happen. `again`: sent from
    /// the frozen pane, which stays frozen, since the FIRST send may still
    /// have worked.
    static func refusalText(_ verdict: CalendarWriteVerdict, _ sent: SentWrite, again: Bool) -> String? {
        let refusal: CalendarRefusal?
        switch verdict {
        case .mayHaveHappened: return nil
        case .notSent: refusal = nil
        case .refused(let r): refusal = r
        }
        if again { return CalendarWrite.resendRefusedText }
        switch sent {
        case .create(let body, _):
            return CalendarWrite.failureText(refusal, .create, guests: !(body.guests ?? []).isEmpty)
        case .edit(_, let patch, _):
            return CalendarWrite.failureText(refusal, .edit, guests: patch.guests != nil)
        }
    }

    /// The line a composer is refused with over a pane that holds it. Only
    /// an editable pane can be saved; any other (sending, frozen) can only
    /// be closed.
    static func composerRefusedText(_ state: EventPaneState) -> String {
        state.fieldsEditable ? "Save or close the event first" : "Close the event first"
    }
}
