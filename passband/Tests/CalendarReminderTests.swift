// Event reminders and the Tickets button (docs/CALENDAR.md, "Reminders and
// tickets"): the `calendar_reminder` frame, the phone's push id, the banner's
// words, where a tap goes, and `tickets` on the timeline.
//
// What is pinned is what would only ever be wrong on someone's lock screen or
// card: a frame a new field breaks, a push read as another account's or as a
// mail event, "in 1 hr" twelve minutes before the doors, a tap that opens the
// wrong thing, and a decoder that quietly drops the new field (a previous one
// did exactly that).

import Foundation

@main
@MainActor
struct CalendarReminderTests {
    static var failures = 0
    static var checks = 0

    static func expect(_ ok: Bool, _ what: String) {
        checks += 1
        if !ok {
            failures += 1
            print("FAIL: \(what)")
        }
    }

    /// The contract's own example, verbatim.
    static let contract = #"""
    {"calendar_reminder": 7, "at": 1793210000,
     "item": {"kind": "event", "calendar_id": "primary", "id": "abc",
              "title": "Phoebe Bridgers", "start": "2026-10-30T20:00:00-07:00",
              "end": null, "all_day": false, "place": "The Greek Theatre"},
     "lead_minutes": 60, "line": "Your 2 tickets are in the Ticketmaster email",
     "open": {"message_id": 41, "thread_id": "t41", "attachment_id": 9},
     "ticket": {"message_id": 41, "thread_id": "t41", "attachment_id": 9,
                "filename": "tickets.pdf", "mime": "application/pdf"}}
    """#

    /// One hour before the contract's start.
    static let hourBefore: Date = EventBanner.eventDate("2026-10-30T19:00:00-07:00")!

    static func main() {
        decodesTheFrame()
        freshness()
        thePushId()
        theBannersWords()
        theEntryCode()
        whereATapGoes()
        ticketsOnTheTimeline()
        theSameFire()
        invisibleMarks()
        print("calendar-reminder: \(checks) checks, \(failures) failures")
        if failures > 0 { exit(1) }
    }

    static func decodesTheFrame() {
        guard let s = CalendarReminderSignal.parse(contract) else {
            return expect(false, "the contract's frame decodes")
        }
        expect(s.calendar_reminder == 7 && s.at == 1_793_210_000, "fire id and stamp")
        expect(s.item.kind == .event && s.item.id == "abc" && s.item.calendar_id == "primary", "the item")
        expect(s.item.title == "Phoebe Bridgers" && s.item.place == "The Greek Theatre", "title and place")
        expect(s.item.agendaId == "e:primary|abc", "the page's row id: \(s.item.agendaId ?? "nil")")
        expect(s.lead_minutes == 60, "lead")
        expect(s.open == .init(message_id: 41, thread_id: "t41", attachment_id: 9), "open")
        expect(s.ticket?.filename == "tickets.pdf" && s.ticket?.mime == "application/pdf", "ticket")

        // A ghost: its id is a number on the wire.
        let ghost = CalendarReminderSignal.parse(
            #"{"calendar_reminder":8,"item":{"kind":"ghost","id":12,"title":"Dinner","start":"2026-10-30T19:30:00-07:00","all_day":false}}"#)
        expect(ghost?.item.id == "12" && ghost?.item.agendaId == "g:12", "a ghost's numeric id")
        expect(ghost?.open == nil && ghost?.ticket == nil && ghost?.line == nil, "nothing optional required")

        // Every optional field of the wrong shape costs only itself.
        let odd = CalendarReminderSignal.parse(
            #"{"calendar_reminder":9,"at":"soon","item":{"kind":"meetup","id":"x","start":"2026-10-30T19:30:00Z","place":5},"lead_minutes":"an hour","line":7,"open":{"thread_id":"t"},"ticket":"yes","extra":{"new":true}}"#)
        expect(odd != nil, "a frame with odd optional fields still decodes")
        expect(odd?.at == nil && odd?.lead_minutes == nil && odd?.line == nil, "the odd fields are nil")
        expect(odd?.open == nil && odd?.ticket == nil && odd?.item.place == nil, "and so are open, ticket, place")
        expect(odd?.item.kind == .other && odd?.item.agendaId == nil, "an unknown kind names no row")

        // The spine is required.
        expect(CalendarReminderSignal.parse(#"{"calendar_reminder":0,"item":{"kind":"event","id":"a","start":"x"}}"#) == nil, "fire id 0")
        expect(CalendarReminderSignal.parse(#"{"calendar_reminder":3,"item":{"kind":"event","id":"a"}}"#) == nil, "no start")
        expect(CalendarReminderSignal.parse(#"{"calendar_reminder":3}"#) == nil, "no item")
        // Not a renotify, an open_draft or a mail event.
        expect(CalendarReminderSignal.parse(#"{"renotify":3,"trigger":"client_online","items":[]}"#) == nil, "renotify is not a reminder")
        expect(RenotifySignal.parse(contract) == nil, "and a reminder is not a renotify")
        expect((try? JSONDecoder().decode(Event.self, from: Data(contract.utf8))) == nil, "nor an event")
    }

    static func freshness() {
        let s = CalendarReminderSignal.parse(contract)!
        expect(s.isFresh(now: 1_793_210_000), "on time")
        expect(s.isFresh(now: 1_793_210_060), "a minute late")
        expect(!s.isFresh(now: 1_793_210_061), "past the minute: dropped")
        expect(!s.isFresh(now: 1_793_209_000), "far ahead of the clock: dropped")
        var undated = s
        undated.at = nil
        expect(undated.isFresh(now: 0), "no stamp: fresh")
    }

    static func thePushId() {
        let account = UUID(uuidString: "6A1F0D8C-2B7E-4E5A-9C3D-1F2E3A4B5C6D")!
        let tag = account.uuidString
        let push = EventBanner.calendarPush("\(tag):cal:7:1793210000")
        expect(push == .init(accountId: account, fireId: 7, fired: 1_793_210_000), "the tagged id")
        expect(EventBanner.calendarPush(tag.lowercased() + ":cal:7:1793210000")?.accountId == account, "a lowercase tag")
        // Tag-less: unroutable on purpose.
        expect(EventBanner.calendarPush("cal:7:1793210000") == nil, "no tag")
        expect(EventBanner.calendarPush(":cal:7:1793210000") == nil, "an empty tag")
        // Malformed.
        for bad in [
            "\(tag):cal:7", "\(tag):cal:7:1793210000:1", "\(tag):cal::1793210000", "\(tag):cal:07:1793210000",
            "\(tag):cal:-7:1793210000", "\(tag):cal:7:+1793210000", "\(tag):cal:x:1793210000",
            "\(tag):Cal:7:1793210000", "\(tag):draft:7:1793210000", "not-a-uuid:cal:7:1793210000",
            "\(tag):cal:7:0", "\(tag):cal:٧:1793210000", "",
        ] {
            expect(EventBanner.calendarPush(bad) == nil, "malformed: \(bad)")
        }
        expect(EventBanner.calendarPush(nil) == nil, "nil")
        // And never read as another kind of push.
        expect(EventBanner.unresolvedPush("\(tag):cal:7:1793210000") == nil, "not a mail event")
        expect(EventBanner.draftPush("\(tag):cal:7:1793210000") == nil, "not a draft")
        expect(EventBanner.calendarPush("\(tag):41") == nil, "a mail event is not a reminder")
        // The raw push routes to the fetch, before any mail route.
        let route = EventBanner.tapRoute(["event_id": "\(tag):cal:7:1793210000"])
        expect(route == .calendarReminderPush(.init(accountId: account, fireId: 7, fired: 1_793_210_000)), "raw push: \(route)")
    }

    static func theEntryCode() {
        let json = #"{"calendar_reminder": 9, "at": 1793210000,"#
            + #""item": {"kind": "event", "calendar_id": "primary", "id": "gym", "title": "Gym session","#
            + #""start": "2026-10-30T20:00:00-07:00", "all_day": false, "place": "Equinox"},"#
            + #""lead_minutes": 10, "line": "If the desk is empty, use the PIN or the intercom","#
            + #""code": {"label": "Door PIN", "value": "642181"}, "open": null, "ticket": null}"#
        let s = CalendarReminderSignal.parse(json)
        expect(s?.code == .init(label: "Door PIN", value: "642181"), "code decodes")
        if let s {
            let body = EventBanner.calendarReminderCopy(s).body
            expect(body == "Door PIN 642181 · If the desk is empty, use the PIN or the intercom", "body: \(body)")
            var codeOnly = s
            codeOnly.line = nil
            expect(EventBanner.calendarReminderCopy(codeOnly).body == "Door PIN 642181", "code alone")
            var bare = s
            bare.code = .init(label: "", value: "642181")
            bare.line = nil
            expect(EventBanner.calendarReminderCopy(bare).body == "642181", "no label")
            var sneaky = s
            sneaky.code = .init(label: "Door\u{202E}PIN", value: "64\u{200B}2181")
            sneaky.line = nil
            expect(EventBanner.calendarReminderCopy(sneaky).body == "DoorPIN 642181", "invisible marks stripped")
        }
        // A code of the wrong shape costs only itself.
        let bad = json.replacingOccurrences(of: #"{"label": "Door PIN", "value": "642181"}"#, with: "42")
        let b = CalendarReminderSignal.parse(bad)
        expect(b != nil && b?.code == nil, "bad code dropped, banner kept")
        // An older daemon sends none.
        let old = json.replacingOccurrences(of: #""code": {"label": "Door PIN", "value": "642181"}, "#, with: "")
        expect(CalendarReminderSignal.parse(old)?.code == nil, "absent code")
    }

    static func theBannersWords() {
        let s = CalendarReminderSignal.parse(contract)!
        let pacific = TimeZone(identifier: "America/Los_Angeles")!
        let copy = EventBanner.calendarReminderCopy(s, now: hourBefore, zone: pacific)
        expect(copy.title == "Phoebe Bridgers", "title")
        expect(copy.subtitle == "8pm · The Greek Theatre", "subtitle: \(copy.subtitle)")
        let eastern = TimeZone(identifier: "America/New_York")!
        expect(
            EventBanner.calendarReminderCopy(s, now: hourBefore, zone: eastern).subtitle == "11pm · The Greek Theatre",
            "the device's own zone")
        expect(
            EventBanner.reminderClock(start: "2026-10-30T08:30:00-07:00", allDay: false, zone: pacific) == "8:30am",
            "minutes shown")
        expect(
            EventBanner.reminderClock(start: "2026-10-30T12:05:00-07:00", allDay: false, zone: pacific) == "12:05pm",
            "noon hour")
        expect(
            EventBanner.reminderClock(start: "2026-10-30T00:00:00-07:00", allDay: false, zone: pacific) == "12am",
            "midnight")
        expect(copy.body == "Your 2 tickets are in the Ticketmaster email", "body")
        expect(copy.sound && copy.threadIdentifier == "passband.calendar", "chimes, grouped")

        func lead(_ seconds: TimeInterval, allDay: Bool = false, start: String = "2026-10-30T20:00:00-07:00", minutes: Int? = 60)
            -> String?
        {
            let start8pm = EventBanner.eventDate("2026-10-30T20:00:00-07:00")!
            return EventBanner.reminderLead(
                start: start, allDay: allDay, leadMinutes: minutes, now: start8pm.addingTimeInterval(-seconds))
        }
        expect(lead(600) == "in 10 min", "10 min")
        expect(lead(1800) == "in 30 min", "30 min")
        expect(lead(3600 - 20) == "in 1 hr", "a few seconds late still reads 1 hr")
        expect(lead(5400) == "in 1 hr 30 min", "90 min")
        expect(lead(7200) == "in 2 hr", "2 hr")
        expect(lead(10800) == "in 3 hr", "3 hr")
        expect(lead(720) == "in 12 min", "a late fire says the truth, not the plan's hour")
        expect(lead(20) == "Starting now", "under a minute")
        expect(lead(-90) == "Starting now", "already begun")
        expect(lead(3600, allDay: true) == "Today", "all day")
        expect(lead(0, start: "garbage", minutes: 30) == "in 30 min", "unreadable start: the lead")
        expect(lead(0, start: "garbage", minutes: nil) == nil, "neither: no lead")

        // All day, no place, no line, no title.
        let bare = CalendarReminderSignal(
            calendar_reminder: 3,
            item: .init(kind: .ghost, id: "4", title: "  ", start: "2026-10-30", all_day: true))
        let bareCopy = EventBanner.calendarReminderCopy(bare, now: hourBefore, zone: pacific)
        expect(bareCopy.subtitle == "Today", "all day, no place: \(bareCopy.subtitle)")
        expect(bareCopy.title == "Untitled event" && bareCopy.body.isEmpty, "fallback title, empty body")

        // Bidi controls and newlines never reach the lock screen.
        let sneaky = CalendarReminderSignal(
            calendar_reminder: 3,
            item: .init(kind: .event, calendar_id: "c", id: "e", title: "Show\u{202E}woh", start: "garbage", place: "Hall\nB"),
            lead_minutes: 10, line: "Bring\u{2066} ID\nand cash")
        let sneakyCopy = EventBanner.calendarReminderCopy(sneaky, now: hourBefore, zone: pacific)
        expect(sneakyCopy.title == "Showwoh", "bidi stripped from the title: \(sneakyCopy.title)")
        expect(sneakyCopy.subtitle == "in 10 min · Hall B", "place flattened: \(sneakyCopy.subtitle)")
        expect(sneakyCopy.body == "Bring ID and cash", "line flattened: \(sneakyCopy.body)")
        for text in [copy.title, copy.subtitle, copy.body, bareCopy.title, EventBanner.calendarFallbackBody] {
            expect(!text.contains("\u{2014}") && !text.contains("\u{2013}"), "no dashes: \(text)")
        }
    }

    static func whereATapGoes() {
        let account = UUID()
        let s = CalendarReminderSignal.parse(contract)!
        let info = EventBanner.calendarReminderUserInfo(s, account: account)
        expect(
            EventBanner.calendarReminderIdentifier(account: account, fire: 7)
                == "passband.calendar.\(account.uuidString).7", "identifier")
        expect(EventBanner.eventIdentity(info) == nil, "no mail-event identity: ToldEvents never swallows it")
        guard case .calendarReminder(let tap) = EventBanner.tapRoute(info) else {
            return expect(false, "the banner routes as a reminder")
        }
        expect(tap.accountId == account && tap.fireId == 7 && tap.itemId == "e:primary|abc", "the tap's spine")
        expect(tap == EventBanner.calendarReminderTap(s, account: account), "the banner and the fetched frame agree")
        let pdf = EventBanner.CalendarFile(
            messageId: 41, threadId: "t41", attachmentId: 9, filename: "tickets.pdf", mime: "application/pdf")
        expect(tap.target(showTickets: true) == .attachment(pdf), "Show tickets: the ticket")
        expect(
            tap.target(showTickets: false)
                == .attachment(.init(messageId: 41, threadId: "t41", attachmentId: 9)), "a tap: open")

        // `open` naming the mail alone opens the reader.
        var mailOnly = s
        mailOnly.open = .init(message_id: 41, thread_id: "t41")
        let mailTap = EventBanner.calendarReminderTap(mailOnly, account: account)
        expect(mailTap.target(showTickets: false) == .mail(.init(messageId: 41, threadId: "t41")), "open: the mail")
        // No `open`: the event's card.
        var cardOnly = s
        cardOnly.open = nil
        cardOnly.ticket = nil
        let cardInfo = EventBanner.calendarReminderUserInfo(cardOnly, account: account)
        guard case .calendarReminder(let cardTap) = EventBanner.tapRoute(cardInfo) else {
            return expect(false, "routes without open")
        }
        expect(
            cardTap.target(showTickets: false) == .event(itemId: "e:primary|abc", start: "2026-10-30T20:00:00-07:00"),
            "no open: the event's card")
        expect(cardTap.target(showTickets: true) == cardTap.target(showTickets: false), "Show tickets with none: a plain tap")
        // An item the page cannot have: nothing but the app.
        var unknown = cardOnly
        unknown.item.kind = .other
        expect(EventBanner.calendarReminderTap(unknown, account: account).target(showTickets: false) == .none, "unknown item")

        // Ints the system wrote back as strings still route.
        var strings = info
        strings[EventBanner.calendarFireKey] = "7"
        strings["\(EventBanner.calendarTicketPrefix).message"] = "41"
        if case .calendarReminder(let t) = EventBanner.tapRoute(strings) {
            expect(t.fireId == 7 && t.ticket?.messageId == 41, "string ints")
        } else {
            expect(false, "string ints route")
        }
        // A reminder that lost its fire id opens nothing, and never mail.
        var lost = info
        lost[EventBanner.calendarFireKey] = nil
        expect(EventBanner.tapRoute(lost) == .thread(nil), "no fire id: nothing")
        // Shown over the app.
        expect(
            EventBanner.shouldPresentForeground(
                appActive: true, windowVisible: true, isTest: false, isAuth: false, isReminder: true),
            "a reminder shows over the app")
        expect(
            !EventBanner.shouldPresentForeground(appActive: true, windowVisible: true, isTest: false, isAuth: false),
            "mail does not")
    }

    static func ticketsOnTheTimeline() {
        let wire = #"""
        {"ghosts":[
          {"id":8,"kind":"ticket","title":"Japanese Breakfast","start":"2026-11-04T20:00:00-08:00",
           "all_day":false,"commitment":"committed","state":"ghost","sources":[],
           "tickets":{"message_id":7,"thread_id":"t7","kind":"ticket",
                      "attachment":{"id":3,"filename":"tickets.pdf","mime":"application/pdf"}}},
          {"id":9,"kind":"travel","title":"UA 1532","start":"2026-11-02T07:05:00-08:00",
           "all_day":false,"commitment":"committed","state":"ghost","sources":[],
           "tickets":{"message_id":8,"thread_id":"t8","kind":"travel",
                      "attachment":{"id":4,"filename":"pass.pkpass","mime":"application/octet-stream"}}},
          {"id":10,"kind":"reservation","title":"Nopa","start":"2026-11-01T19:30:00-08:00",
           "all_day":false,"commitment":"committed","state":"ghost","sources":[],
           "tickets":"broken"},
          {"id":11,"kind":"reservation","title":"Hotel","start":"2026-11-01","all_day":true,
           "commitment":"committed","state":"ghost","sources":[],"tickets":null}],
         "events":[
          {"id":"e1","calendar_id":"primary","title":"Phoebe Bridgers","start":"2026-10-30T20:00:00-07:00",
           "all_day":false,"tickets":{"message_id":41,"thread_id":"t41","kind":"reservation","attachment":null}},
          {"id":"e2","calendar_id":"primary","title":"Odd","start":"2026-10-31T20:00:00-07:00",
           "all_day":false,"tickets":{"message_id":42,"thread_id":"t42","kind":"spaceship","attachment":{"id":"x"}}},
          {"id":"e3","calendar_id":"primary","title":"Broken","start":"2026-10-31T21:00:00-07:00",
           "all_day":false,"tickets":{"thread_id":"t43"}},
          {"id":"e4","calendar_id":"primary","title":"None","start":"2026-10-31T22:00:00-07:00","all_day":false}]}
        """#
        guard let feed = try? JSONDecoder().decode(TimelineFeed.self, from: Data(wire.utf8)) else {
            return expect(false, "the feed decodes")
        }
        expect(feed.ghosts.count == 4, "every ghost kept, a broken tickets included: \(feed.ghosts.count)")
        expect(feed.events?.count == 4, "every event kept: \(feed.events?.count ?? -1)")
        let g = Dictionary(uniqueKeysWithValues: feed.ghosts.map { ($0.id, $0) })
        let e = Dictionary(uniqueKeysWithValues: (feed.events ?? []).map { ($0.id, $0) })

        expect(
            g[8]?.tickets
                == CalendarTickets(
                    message_id: 7, thread_id: "t7", kind: .ticket,
                    attachment: .init(id: 3, filename: "tickets.pdf", mime: "application/pdf")),
            "a ghost's tickets decode whole")
        expect(g[8]?.tickets?.label == "Tickets", "ticket: Tickets")
        expect(g[9]?.tickets?.label == "Boarding pass", "travel: Boarding pass")
        expect(g[9]?.tickets?.attachment?.isWalletPass == true, "a .pkpass by its name")
        expect(g[10]?.tickets == nil && g[11]?.tickets == nil, "broken and null: nil, ghost kept")

        expect(e["e1"]?.tickets?.label == "Booking" && e["e1"]?.tickets?.attachment == nil, "an event's mail-only booking")
        expect(e["e2"]?.tickets?.message_id == 42, "an odd kind and attachment cost only themselves")
        expect(e["e2"]?.tickets?.label == "Tickets" && e["e2"]?.tickets?.attachment == nil, "the odd parts read plain")
        expect(e["e3"]?.tickets == nil && e["e3"]?.title == "Broken", "no message id: no tickets, event kept")
        expect(e["e4"]?.tickets == nil, "absent: nil")

        // Through the row the card reads.
        let cal = Calendar(identifier: .gregorian)
        expect(Agenda.item(g[8]!, calendar: cal)?.tickets?.message_id == 7, "the ghost's card has its tickets")
        expect(Agenda.item(e["e1"]!, calendar: cal)?.tickets?.message_id == 41, "the event's card has its tickets")

        // The wire's own shape back out: what the disk cache writes.
        guard let again = try? JSONDecoder().decode(TimelineFeed.self, from: JSONEncoder().encode(feed)) else {
            return expect(false, "the feed round-trips")
        }
        expect(again == feed, "tickets survive an encode and decode")
    }

    /// A push names its fire by id AND second: a late tap on an old
    /// undressed banner must never open a different reminder under that id.
    static func theSameFire() {
        guard let signal = CalendarReminderSignal.parse(contract) else {
            return expect(false, "the contract decodes")
        }
        expect(EventBanner.isPushedFire(signal, fireId: 7, fired: 1793210000), "the same fire")
        expect(!EventBanner.isPushedFire(signal, fireId: 7, fired: 1793210001), "another second")
        expect(!EventBanner.isPushedFire(signal, fireId: 8, fired: 1793210000), "another id")
        var undated = signal
        undated.at = nil
        expect(!EventBanner.isPushedFire(undated, fireId: 7, fired: 1793210000), "no date is no match")
    }

    /// The marks `stripBidi` leaves are gone from a reminder's words too;
    /// an emoji's joiner stays.
    static func invisibleMarks() {
        let raw = "Gate\u{200E} B12\u{200F}\u{061C}\u{200B}\u{2060}\u{FEFF}\u{E0041}\u{202E}!"
        expect(EventBanner.stripInvisible(raw) == "Gate B12!", "marks gone: \(EventBanner.stripInvisible(raw))")
        let family = "\u{1F468}\u{200D}\u{1F469}"
        expect(EventBanner.stripInvisible(family) == family, "a joiner stays")
        var signal = CalendarReminderSignal.parse(contract)!
        signal.item.title = "Phoebe\u{200B} Bridgers\u{200E}"
        signal.item.place = "The\u{2060} Greek"
        let copy = EventBanner.calendarReminderCopy(signal, now: hourBefore)
        expect(copy.title == "Phoebe Bridgers", "title: \(copy.title)")
        expect(copy.subtitle.hasSuffix("The Greek"), "place: \(copy.subtitle)")
    }
}
