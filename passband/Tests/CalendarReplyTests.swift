// Answering invitations (Lib/CalendarReply.swift): reading the Yes / No /
// Maybe links in Google's invitation email, finding the event they name on
// the synced calendar, and which events an answer covers. A wrong answer here
// is a real "Declined" mailed to a real organizer, so the links that must NOT
// be read (and so open in the browser as before) matter as much as the ones
// that must.

import Foundation

@main
struct CalendarReplyTests {
    nonisolated(unsafe) static var failures = 0
    nonisolated(unsafe) static var checks = 0

    static var cal: Calendar {
        var c = Calendar(identifier: .gregorian)
        c.timeZone = TimeZone(identifier: "America/Los_Angeles")!
        return c
    }
    static let now = AgendaTime.parse("2026-11-10T12:00:00-08:00", zone: nil, calendar: cal)!.date

    static func main() {
        linksRead()
        linksRefused()
        linksLand()
        seriesDefaults()
        words()

        if failures > 0 {
            print("FAILED: \(failures) of \(checks) checks")
            exit(1)
        }
        print("ok: \(checks) checks passed")
    }

    static func eid(_ s: String, urlSafe: Bool = false, padded: Bool = true) -> String {
        var e = Data(s.utf8).base64EncodedString()
        if urlSafe { e = e.replacingOccurrences(of: "+", with: "-").replacingOccurrences(of: "/", with: "_") }
        if !padded { e = e.replacingOccurrences(of: "=", with: "") }
        return e
    }

    static func link(_ eid: String, rst: String = "1", host: String = "calendar.google.com") -> String {
        "https://\(host)/calendar/event?action=RESPOND&eid=\(eid)&rst=\(rst)&tok=MTkj&ctz=America%2FLos_Angeles&hl=en&es=0"
    }

    static func event(
        _ id: String, calendar: String = "primary", start: String = "2026-11-12T10:00:00-08:00",
        response: TimelineSelfResponse? = .needsAction, respondable: Bool? = true, recurring: Bool? = nil
    ) -> TimelineEvent {
        var e = TimelineEvent(
            id: id, calendar_id: calendar, calendar_name: "Personal", title: "Party", start: start,
            end: nil, all_day: false, start_timezone: nil, location: nil,
            status: .confirmed, self_response: response, busy: true, organizer: "host@example.com",
            html_link: nil, color: nil, editable: false, description: nil)
        e.respondable = respondable
        e.recurring = recurring
        return e
    }

    // MARK: links

    static func linksRead() {
        let yes = InviteLink.read(link(eid("abc123 me@example.com")))
        expect(yes == InviteLink(eventId: "abc123", calendar: "me@example.com", answer: .accepted), "rst=1 is yes")
        expect(InviteLink.read(link(eid("abc123 me@example.com"), rst: "2"))?.answer == .declined, "rst=2 is no")
        expect(InviteLink.read(link(eid("abc123 me@example.com"), rst: "3"))?.answer == .tentative, "rst=3 is maybe")
        // Google's eids come padded, unpadded and URL-safe.
        let tricky = "abc~?>123 me@example.com"
        expect(
            InviteLink.read(link(eid(tricky, urlSafe: true, padded: false)))?.calendar == "me@example.com",
            "a URL-safe, unpadded eid reads")
        expect(
            InviteLink.read(link(eid("abc123 me@example.com"), host: "www.google.com"))?.eventId == "abc123",
            "the older www.google.com/calendar link")
        expect(InviteLink.read(link(eid("abc123")))?.calendar == nil, "an eid with no calendar")
        let multi = "https://calendar.google.com/calendar/u/0/event?ACTION=respond&eid=\(eid("x1 a@b.c"))&rst=1"
        expect(InviteLink.read(multi)?.eventId == "x1", "a signed-in /u/0/ path, any case of action")
    }

    static func linksRefused() {
        let e = eid("abc123 me@example.com")
        for (raw, why) in [
            ("http://calendar.google.com/calendar/event?action=RESPOND&eid=\(e)&rst=1", "not https"),
            ("https://calendar.google.com.evil.example/calendar/event?action=RESPOND&eid=\(e)&rst=1", "a lookalike host"),
            ("https://evil.example/calendar/event?action=RESPOND&eid=\(e)&rst=1", "another host"),
            ("https://calendar.google.com/calendar/event?action=VIEW&eid=\(e)", "a view link, not an answer"),
            ("https://calendar.google.com/calendar/event?action=RESPOND&eid=\(e)", "no answer"),
            ("https://calendar.google.com/calendar/event?action=RESPOND&eid=\(e)&rst=4", "an unknown answer"),
            ("https://calendar.google.com/calendar/event?action=RESPOND&rst=1", "no eid"),
            ("https://calendar.google.com/calendar/event?action=RESPOND&eid=%%%&rst=1", "an eid that is not base64"),
            ("https://calendar.google.com/calendar/r/eventedit?action=RESPOND&eid=\(e)&rst=1", "another page"),
            ("mailto:host@example.com", "not a link at all"),
        ] {
            expect(InviteLink.read(raw) == nil, "refused: \(why)")
        }
    }

    static func linksLand() {
        let party = event("party")
        let other = event("party", calendar: "team@group.calendar.google.com")
        let mine = event("own", response: nil, respondable: false)
        let weekly = [
            event("weekly_20261103T180000Z", start: "2026-11-03T10:00:00-08:00", recurring: true),
            event("weekly_20261117T180000Z", start: "2026-11-17T10:00:00-08:00", recurring: true),
            event("weekly_20261110T200000Z", start: "2026-11-10T12:00:00-08:00", recurring: true),
        ]
        let feed = [other, party, mine] + weekly
        func land(_ id: String, _ calendar: String? = "me@example.com", events: [TimelineEvent] = feed) -> (String, String, Bool)? {
            CalendarReply.landing(
                InviteLink(eventId: id, calendar: calendar, answer: .accepted), events: events,
                accountEmail: "Me@Example.com", now: now, calendar: cal
            ).map { ($0.event.calendar_id, $0.event.id, $0.series) }
        }
        expect(land("party").map { $0.0 == "primary" && $0.1 == "party" && !$0.2 } == true,
            "the invited address is the primary calendar, read as \"primary\"")
        expect(land("party", "team@group.calendar.google.com")?.0 == "team@group.calendar.google.com",
            "a calendar named by its id")
        expect(land("party", nil)?.1 == "party", "no calendar in the link: the event by id")
        expect(land("own") == nil, "the user's own event is not answered")
        expect(land("ghost") == nil, "an event not on the calendar opens in the browser")
        let series = land("weekly")
        expect(series?.1 == "weekly_20261110T200000Z" && series?.2 == true,
            "a series link answers the series, through its next instance not over")
        expect(land("weekly_20261117T180000Z").map { $0.1 == "weekly_20261117T180000Z" && !$0.2 } == true,
            "an instance link answers that instance")
        expect(land("party", events: [event("party", respondable: nil)]) == nil, "an older daemon: no answering")
    }

    static func seriesDefaults() {
        expect(CalendarReply.defaultsToSeries(event("w_20261110", recurring: true)),
            "an unanswered recurring invitation: all of them")
        expect(!CalendarReply.defaultsToSeries(event("w_20261110", response: .accepted, recurring: true)),
            "already answered: just this one")
        expect(!CalendarReply.defaultsToSeries(event("party")), "not recurring")
        expect(CalendarReply.series(of: "abc_20261110T170000Z") == "abc", "a timed instance")
        expect(CalendarReply.series(of: "abc_20261110") == "abc", "an all-day instance")
        expect(CalendarReply.series(of: "abc") == nil, "not an instance")
        expect(CalendarReply.series(of: "abc_123") == nil, "not a stamp")
        expect(CalendarReply.series(of: "_20261110") == nil, "no series")
        expect(
            CalendarReply.path(calendarId: "team@group.calendar.google.com", eventId: "x")
                .hasSuffix("/x/reply"), "the reply route")
    }

    static func words() {
        expect(
            CalendarReply.doneText(.accepted, series: false, organizer: "host@example.com")
                == "Going. Google Calendar let host@example.com know.", "a yes")
        expect(
            CalendarReply.doneText(.declined, series: true, organizer: nil)
                == "Declined all of them. Google Calendar let the organizer know.", "a series no")
        expect(!CalendarReply.doneText(.tentative, series: false, organizer: "").contains("  "), "no empty organizer")
        let body = try? JSONEncoder().encode(ReplyWrite(.tentative, series: true))
        let json = body.flatMap { try? JSONSerialization.jsonObject(with: $0) as? [String: Any] }
        expect(json?["response"] as? String == "tentative", "Google's word")
        expect(json?["notify"] as? Bool == true, "the organizer is always told")
        expect(json?["series"] as? Bool == true, "the series as asked")
        for text in [CalendarReply.uncertainText, CalendarReply.failureText(nil), CalendarReply.failureText(.notInvited)] {
            expect(!text.contains("\u{2014}"), "no em dash: \(text)")
        }
    }

    static func expect(_ cond: Bool, _ what: String) {
        checks += 1
        if !cond {
            failures += 1
            print("  FAIL: \(what)")
        }
    }
}
