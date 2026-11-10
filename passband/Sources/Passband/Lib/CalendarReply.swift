// ANSWERING AN INVITATION: yes, no or maybe to an event someone else sent,
// through Google Calendar, which tells the organizer (docs/CALENDAR.md §8,
// "Replies"). The daemon sends only the user's own answer
// (`POST /client/calendar/events/{calendar_id}/{id}/reply`); nothing here
// writes mail.
//
// TWO WAYS IN. The event's detail on the Calendar page (y / x and the
// buttons), and the Yes / No / Maybe links in Google's own invitation email.
// Those links open a browser tab to answer; Passband reads them instead
// (`InviteLink`), finds the event on the synced calendar, and answers it in
// place. A link Passband cannot place on the calendar still opens in the
// browser, as before.
//
// PURE, Foundation only, so test.sh holds it (Tests/CalendarReplyTests.swift).

import Foundation

/// The three answers. Google's own words on the wire.
enum ReplyAnswer: String, CaseIterable, Hashable, Sendable {
    case accepted, declined, tentative

    /// The selected-state word on the detail ("Going", "Not going", "Maybe").
    var label: String {
        switch self {
        case .accepted: "yes"
        case .declined: "no"
        case .tentative: "maybe"
        }
    }

    /// The answer as the event already holds it, if it is one of these.
    init?(_ response: TimelineSelfResponse?) {
        switch response {
        case .accepted: self = .accepted
        case .declined: self = .declined
        case .tentative: self = .tentative
        case .needsAction, nil: return nil
        }
    }
}

/// The body the daemon is sent. `notify` is always true: a reply the
/// organizer never hears is not a reply.
struct ReplyWrite: Encodable, Hashable, Sendable {
    let response: String
    let series: Bool
    let notify: Bool

    init(_ answer: ReplyAnswer, series: Bool) {
        response = answer.rawValue
        self.series = series
        notify = true
    }
}

/// One Yes / No / Maybe link from Google's invitation email:
/// `https://calendar.google.com/calendar/event?action=RESPOND&eid=…&rst=1&tok=…`.
/// `eid` is base64 of "<event id> <calendar id>"; `rst` is 1 yes, 2 no, 3
/// maybe. The `tok` that lets the link answer without a sign-in is never
/// read: the answer goes through the user's own calendar grant.
struct InviteLink: Hashable, Sendable {
    let eventId: String
    /// The calendar the invitation went to: usually the invited address.
    let calendar: String?
    let answer: ReplyAnswer

    static func read(_ raw: String) -> InviteLink? {
        guard let url = URLComponents(string: raw.trimmingCharacters(in: .whitespacesAndNewlines)),
            url.scheme?.lowercased() == "https",
            let host = url.host?.lowercased(),
            host == "calendar.google.com" || host == "www.google.com",
            url.path.hasPrefix("/calendar/"), url.path.hasSuffix("/event")
        else { return nil }
        let items = url.queryItems ?? []
        func value(_ name: String) -> String? {
            items.first { $0.name.lowercased() == name }?.value
        }
        guard value("action")?.uppercased() == "RESPOND", let eid = value("eid"),
            let decoded = decodeEid(eid)
        else { return nil }
        let answer: ReplyAnswer
        switch value("rst") {
        case "1": answer = .accepted
        case "2": answer = .declined
        case "3": answer = .tentative
        default: return nil
        }
        let parts = decoded.split(whereSeparator: \.isWhitespace).map(String.init)
        guard let id = parts.first, !id.isEmpty else { return nil }
        return InviteLink(eventId: id, calendar: parts.count > 1 ? parts[1] : nil, answer: answer)
    }

    /// Base64, either alphabet, padded or not.
    static func decodeEid(_ eid: String) -> String? {
        var s = eid.replacingOccurrences(of: "-", with: "+").replacingOccurrences(of: "_", with: "/")
        while s.count % 4 != 0 { s += "=" }
        guard let data = Data(base64Encoded: s), let text = String(data: data, encoding: .utf8) else { return nil }
        return text
    }
}

/// Where an answer was given from, for analytics: a key, a button on the
/// detail, or a link in the invitation email.
enum ReplyVia: String, Sendable {
    case key, click, mail
}

enum CalendarReply {
    /// The daemon says the user is a guest who may answer. Nil (an older
    /// daemon) is no.
    static func canReply(_ event: TimelineEvent) -> Bool { event.respondable == true }

    /// The route for one event.
    static func path(calendarId: String, eventId: String) -> String {
        CalendarWrite.path(calendarId: calendarId, eventId: eventId) + "/reply"
    }

    /// WHICH EVENTS AN ANSWER COVERS, by default: the whole series for a
    /// recurring invitation not answered yet (saying yes to a weekly 1:1 is
    /// saying yes to all of it), only this one once it has been answered
    /// (declining next Tuesday is not quitting the meeting). The detail
    /// offers the other.
    static func defaultsToSeries(_ event: TimelineEvent) -> Bool {
        event.recurring == true && ReplyAnswer(event.self_response) == nil
    }

    /// The series an instance belongs to, read from its id the way Google
    /// names instances: "<series>_<YYYYMMDD>" or "<series>_<YYYYMMDDTHHMMSSZ>".
    static func series(of eventId: String) -> String? {
        guard let cut = eventId.lastIndex(of: "_") else { return nil }
        let series = String(eventId[..<cut])
        let stamp = Array(eventId[eventId.index(after: cut)...])
        let digits = { (s: ArraySlice<Character>) in s.allSatisfy { $0.isASCII && $0.isNumber } }
        let ok =
            (stamp.count == 8 && digits(stamp[0..<8]))
            || (stamp.count == 16 && digits(stamp[0..<8]) && stamp[8] == "T" && digits(stamp[9..<15])
                && stamp[15] == "Z")
        return ok && !series.isEmpty ? series : nil
    }

    /// WHERE AN EMAIL'S LINK LANDS on the synced calendar: the event it names
    /// (on the calendar it names, when that is one being read: the primary
    /// calendar is read as "primary", so the invited address is taken to be
    /// it), and whether the answer is for the whole series. A link naming a
    /// SERIES (a recurring invitation) answers it through its next instance
    /// not yet over. Nil when the event is not on the calendar, or not one
    /// the user may answer: the link then opens in the browser as before.
    static func landing(
        _ link: InviteLink, events: [TimelineEvent], accountEmail: String?, now: Date,
        calendar cal: Calendar = .current
    ) -> (event: TimelineEvent, series: Bool)? {
        let calendar = link.calendar?.lowercased()
        let mailbox = accountEmail?.lowercased()
        func onItsCalendar(_ e: TimelineEvent) -> Bool {
            guard let calendar else { return true }
            let id = e.calendar_id.lowercased()
            return id == calendar || (id == "primary" && (mailbox == nil || calendar == mailbox))
        }
        let replyable = events.filter(canReply)
        let ranked = replyable.filter(onItsCalendar) + replyable.filter { !onItsCalendar($0) }
        if let exact = ranked.first(where: { $0.id == link.eventId }) {
            return (exact, false)
        }
        let instances = ranked.filter { series(of: $0.id) == link.eventId }
        guard !instances.isEmpty else { return nil }
        func at(_ raw: String) -> Date {
            AgendaTime.parse(raw, zone: nil, calendar: cal)?.date ?? .distantFuture
        }
        let upcoming = instances.filter { at($0.end ?? $0.start) >= now }
        let pick = (upcoming.isEmpty ? instances : upcoming).min { at($0.start) < at($1.start) }
        return pick.map { ($0, true) }
    }

    /// The toast once Google has the answer.
    static func doneText(_ answer: ReplyAnswer, series: Bool, organizer: String?) -> String {
        let said: String
        switch answer {
        case .accepted: said = series ? "Going to all of them" : "Going"
        case .declined: said = series ? "Declined all of them" : "Declined"
        case .tentative: said = series ? "Maybe to all of them" : "Maybe"
        }
        let who = organizer.flatMap { $0.isEmpty ? nil : $0 } ?? "the organizer"
        return "\(said). Google Calendar let \(who) know."
    }

    /// A reply that may or may not have reached Google.
    static let uncertainText = "Google Calendar may have your answer. Check before answering again."

    /// A refusal, said plainly. Nothing was sent.
    static func failureText(_ refusal: CalendarRefusal?) -> String {
        switch refusal {
        case nil: "Could not reach your Passband server, so your answer was not sent."
        case .notInvited: "You are not a guest of that event, so there is nothing to answer."
        case .notFound: "That event is no longer on your calendar."
        case .notGranted: "Google Calendar is not connected, so your answer was not sent."
        case .googleRefused: "Google Calendar refused, so your answer was not sent."
        default: "Passband could not send that answer."
        }
    }
}
