// The search panel's Events block (Lib/CalendarSearch.swift): which query goes
// to which search, the wire it reads, how many rows show, and what each says.
// Fixture clock: Wednesday October 28, 2026, 3pm in San Francisco.

import Foundation

@main
struct CalendarSearchTests {
    nonisolated(unsafe) static var failures = 0
    nonisolated(unsafe) static var checks = 0

    static var cal: Calendar {
        var c = Calendar(identifier: .gregorian)
        c.timeZone = TimeZone(identifier: "America/Los_Angeles")!
        return c
    }
    static let now = AgendaTime.parse("2026-10-28T15:00:00-07:00", zone: nil, calendar: cal)!.date

    static func main() {
        theScopeOperatorIsLiftedOut()
        onlyQueriesWithWordsAskForEvents()
        thePageDecodesLeniently()
        rowsKeepTheDaemonsOrder()
        theBlockShowsThreeThenTheRest()
        rowsSayWhenAndWhere()

        if failures > 0 {
            print("FAILED: \(failures) of \(checks) checks")
            exit(1)
        }
        print("ok: \(checks) checks passed")
    }

    static func expect(_ ok: Bool, _ what: String) {
        checks += 1
        if !ok {
            failures += 1
            print("FAIL: \(what)")
        }
    }

    static func page(_ json: String) -> CalendarSearchPage {
        try! JSONDecoder().decode(CalendarSearchPage.self, from: Data(json.utf8))
    }

    static let event = """
        {"event":{"id":"e1","calendar_id":"primary","title":"Dinner w/ Sam","start":"2026-11-05T19:30:00-08:00",
        "end":"2026-11-05T21:00:00-08:00","all_day":false,"location":"Nopa","color":"#7986cb"}}
        """
    static let ghost = """
        {"ghost":{"id":7,"kind":"invite","title":"Sam's birthday","start":"2026-11-14","all_day":true,
        "commitment":"invited","state":"upcoming","sources":[{"message_id":3,"thread_id":"t3","role":"origin"}]}}
        """
    static let past = """
        {"event":{"id":"e0","calendar_id":"primary","title":"Dinner w/ Sam","start":"2025-09-12T09:00:00-07:00",
        "all_day":false}}
        """

    static func theScopeOperatorIsLiftedOut() {
        expect(CalendarSearch.scope("in:calendar dinner") == (true, "dinner"), "leading operator")
        expect(CalendarSearch.scope("dinner  IN:Calendar sam") == (true, "dinner sam"), "any case, any place")
        expect(CalendarSearch.scope("dinner sam") == (false, "dinner sam"), "absent")
        expect(CalendarSearch.eventsOnly("in:calendar"), "alone")
        expect(!CalendarSearch.eventsOnly("in:calendars"), "a longer word is not the operator")
    }

    static func onlyQueriesWithWordsAskForEvents() {
        expect(CalendarSearch.eventQuery("dinner sam") == "dinner sam", "words go")
        expect(CalendarSearch.eventQuery("in:calendar dinner") == "dinner", "the scope never reaches the daemon")
        expect(CalendarSearch.eventQuery("in:calendar") == nil, "the scope alone is not a question")
        expect(CalendarSearch.eventQuery("d") == nil, "one character")
        expect(CalendarSearch.eventQuery("from:sam dinner") == nil, "from: is about mail")
        expect(CalendarSearch.eventQuery("to:dan lease") == nil, "to: is about mail")
        expect(CalendarSearch.eventQuery("in:sent lease") == nil, "in:sent is about mail")
        expect(CalendarSearch.eventQuery("after:2026-01-01") == nil, "dates alone are no words")
        expect(CalendarSearch.eventQuery("dinner after:2026-01-01") == "dinner after:2026-01-01",
            "dates bound the start, and travel with the words")
    }

    static func thePageDecodesLeniently() {
        let p = page(#"{"items":[\#(event),{"event":{"id":1}},\#(ghost)],"total":9,"granted":true}"#)
        expect(p.items.count == 2, "a bad row costs only itself")
        expect(p.items[0].event?.id == "e1" && p.items[1].ghost?.id == 7, "both kinds")
        expect(p.total == 9 && p.granted, "total and grant")
        let bare = page(#"{"items":[\#(event)]}"#)
        expect(bare.total == 1 && !bare.granted, "missing total is the rows, missing grant is no")
    }

    static func rowsKeepTheDaemonsOrder() {
        let rows = CalendarSearch.items(page(#"{"items":[\#(event),\#(ghost),\#(past)],"total":3}"#), calendar: cal)
        expect(rows.map(\.id) == ["e:primary|e1", "g:7", "e:primary|e0"], "upcoming, then past, as served")
        expect(!CalendarSearch.isPast(rows[0], now: now) && CalendarSearch.isPast(rows[2], now: now), "past")
        expect(rows[1].isGhost, "a ghost stays a ghost")
    }

    static func theBlockShowsThreeThenTheRest() {
        let rows = CalendarSearch.items(
            page(#"{"items":[\#(event),\#(ghost),\#(past),\#(event.replacingOccurrences(of: "e1", with: "e2"))]}"#),
            calendar: cal)
        let shut = CalendarSearch.block(rows, total: 12, expanded: false)
        expect(shut.shown.count == 3 && shut.more == 9, "three, and the daemon's count of the rest")
        let open = CalendarSearch.block(rows, total: 12, expanded: true)
        expect(open.shown.count == 4 && open.more == 0, "expanded shows what came back")
        let small = CalendarSearch.block(Array(rows.prefix(2)), total: 2, expanded: false)
        expect(small.shown.count == 2 && small.more == 0, "nothing more")
        let under = CalendarSearch.block(rows, total: 1, expanded: false)
        expect(under.more == 1, "an undercounting total never hides a fetched row")
        expect(CalendarSearch.moreLabel(1) == "1 more event" && CalendarSearch.moreLabel(4) == "4 more events",
            "more label")
    }

    static func rowsSayWhenAndWhere() {
        let rows = CalendarSearch.items(page(#"{"items":[\#(event),\#(ghost),\#(past)]}"#), calendar: cal)
        expect(CalendarSearch.label(rows[0], now: now, calendar: cal) == "Thu Nov 5 · 7:30pm · Nopa",
            "timed, in the user's zone, with the place: \(CalendarSearch.label(rows[0], now: now, calendar: cal))")
        expect(CalendarSearch.label(rows[1], now: now, calendar: cal) == "Sat Nov 14 · all day", "all day")
        expect(CalendarSearch.label(rows[2], now: now, calendar: cal) == "Sep 12, 2025 · 9am", "another year")
        expect(CalendarSearch.day(rows[0], calendar: cal) == cal.startOfDay(for: rows[0].start), "opens on its day")
    }
}
