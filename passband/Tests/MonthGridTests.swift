// The month view's arithmetic, against fixtures (Lib/MonthGrid.swift).
//
// Each of these is wrong only on somebody's screen in a particular month: the
// grid a row short in a month that starts on a Sunday, the party that
// vanishes at midnight instead of landing in Sunday's night cap, every block a
// sliver to the left on the Sunday the clocks change, the trip that loses its
// name on the second row it crosses. The fixture month is the mock's
// (docs/calendar-ux-preview.html): October 2026 in San Francisco, today
// Monday October 5 at 3:40pm.

import Foundation

@main
struct MonthGridTests {
    nonisolated(unsafe) static var failures = 0
    nonisolated(unsafe) static var checks = 0

    static let zone = TimeZone(identifier: "America/Los_Angeles")!
    /// Monday first, as the mock draws it.
    static var cal: Calendar { calendar(firstWeekday: 2) }
    static func calendar(firstWeekday: Int) -> Calendar {
        var c = Calendar(identifier: .gregorian)
        c.timeZone = zone
        c.firstWeekday = firstWeekday
        return c
    }
    /// Monday Oct 5 2026, 3:40pm PDT.
    static let now = at("2026-10-05T15:40:00-07:00")

    static func main() {
        aMonthThatStartsOnMonday()
        aMonthThatNeedsSixRows()
        theUsersFirstWeekday()
        gridDaysAreMidnightsAcrossDST()
        sevenAmIsTheStripsEdge()
        aPartyRunsOffSaturdayIntoSundaysNightCap()
        theAxisIsWallTimeOnDSTWeekends()
        spansAcrossWeekRows()
        whatIsAndIsNotASpan()
        anAllDaySingleDayIsABarNotABlock()
        spanLanesAndOverflow()
        unlikelyIsCountedNotDrawn()
        theDayColumn()
        columnLanes()
        columnTimes()
        arrowsAndMonths()
        theRequestedRange()
        nowOnTheStrip()

        if failures > 0 {
            print("FAILED: \(failures) of \(checks) checks")
            exit(1)
        }
        print("ok: \(checks) checks passed")
    }

    // MARK: fixtures

    static func at(_ s: String) -> Date {
        AgendaTime.parse(s, zone: nil, calendar: cal)!.date
    }

    static func day(_ s: String, _ c: Calendar = cal) -> Date {
        c.startOfDay(for: at(s + "T12:00:00-07:00"))
    }

    static func month(_ s: String) -> Date { MonthGrid.monthStart(day(s), calendar: cal) }

    static func ghost(
        _ id: Int, _ title: String, start: String, end: String? = nil, allDay: Bool = false,
        estimated: Bool = false, commitment: GhostCommitment = .invited,
        band: GhostBand? = nil
    ) -> TimelineGhost {
        TimelineGhost(
            id: id, kind: .invite, title: title, start: start,
            start_timezone: "America/Los_Angeles", all_day: allDay, end: end,
            end_estimated: estimated, end_source: estimated ? .model : (end == nil ? nil : .stated),
            estimate: nil, location: nil, commitment: commitment, state: .ghost, mail_state: .live,
            verified: true,
            likelihood: band.map { GhostLikelihood(band: $0, reason: nil) },
            sources: [GhostSource(message_id: id, thread_id: "t\(id)", role: .created)],
            matched_event: nil)
    }

    static func event(
        _ id: String, _ title: String, start: String, end: String? = nil, allDay: Bool = false
    ) -> TimelineEvent {
        TimelineEvent(
            id: id, calendar_id: "primary", calendar_name: "Work", title: title, start: start,
            end: end, all_day: allDay, start_timezone: nil, location: nil, status: .confirmed,
            self_response: nil, busy: true, organizer: nil, html_link: nil)
    }

    static func items(_ ghosts: [TimelineGhost], _ events: [TimelineEvent] = []) -> [AgendaItem] {
        Agenda.items(TimelineFeed(ghosts: ghosts, events: events), calendar: cal)
    }

    static func build(_ ghosts: [TimelineGhost], _ events: [TimelineEvent] = [], month m: String = "2026-10-15")
        -> MonthModel
    {
        MonthGrid.build(items(ghosts, events), month: month(m), calendar: cal)
    }

    static func cell(_ model: MonthModel, _ d: String) -> MonthDay? {
        model.days.first { $0.day == day(d) }
    }

    static func near(_ a: Double, _ b: Double) -> Bool { abs(a - b) < 1e-9 }

    // MARK: the grid

    /// June 2026 starts on a Monday: no greyed days ahead of it, five rows.
    /// February 2027 starts on a Monday and has 28 days: exactly four rows,
    /// nothing greyed at all.
    static func aMonthThatStartsOnMonday() {
        let june = MonthGrid.gridDays(month: month("2026-06-10"), calendar: cal)
        expect(june.first == day("2026-06-01"), "June 2026's grid starts on June 1, a Monday")
        expect(june.count == 35, "June 2026 is five rows")
        expect(june.last == day("2026-07-05"), "and ends on the Sunday after the 30th")
        let feb = MonthGrid.gridDays(month: month("2027-02-10"), calendar: cal)
        expect(feb.count == 28 && feb.first == day("2027-02-01") && feb.last == day("2027-02-28"),
               "February 2027 is exactly four rows, Feb 1 to Feb 28")
        let model = MonthGrid.build([], month: month("2027-02-10"), calendar: cal)
        expect(model.weeks.count == 4 && model.days.allSatisfy(\.inMonth),
               "four rows, none greyed")
    }

    /// March 2026 starts on a Sunday: six greyed days ahead of it and 31 days
    /// after, so six rows. October 2026 (the mock) is five, starting Sep 28.
    static func aMonthThatNeedsSixRows() {
        let march = MonthGrid.gridDays(month: month("2026-03-10"), calendar: cal)
        expect(march.count == 42, "March 2026 needs six rows")
        expect(march.first == day("2026-02-23") && march.last == day("2026-04-05"),
               "Feb 23 to Apr 5")
        let oct = MonthGrid.build([], month: month("2026-10-15"), calendar: cal)
        expect(oct.weeks.count == 5, "October 2026 is five rows")
        expect(oct.days.first?.day == day("2026-09-28") && oct.days.first?.inMonth == false,
               "starting on a greyed Sep 28")
        expect(oct.days.last?.day == day("2026-11-01") && oct.days.last?.inMonth == false,
               "and ending on a greyed Nov 1")
        expect(MonthGrid.cellLabel(day("2026-09-28"), month: oct.month, calendar: cal) == "Sep 28",
               "a greyed day names its month")
        expect(MonthGrid.cellLabel(day("2026-10-01"), month: oct.month, calendar: cal) == "Oct 1",
               "so does the 1st")
        expect(MonthGrid.cellLabel(day("2026-10-08"), month: oct.month, calendar: cal) == "8",
               "any other day is its number")
        expect(MonthGrid.title(oct.month, calendar: cal) == "October 2026", "the header")
    }

    /// The first weekday is the user's calendar's, not a constant.
    static func theUsersFirstWeekday() {
        let sunday = calendar(firstWeekday: 1)
        let oct = MonthGrid.gridDays(month: month("2026-10-15"), calendar: sunday)
        expect(oct.first == day("2026-09-27", sunday), "Sunday first starts October on Sep 27")
        expect(oct.count == 35, "still five rows")
        expect(MonthGrid.weekdayLabels(sunday).first == "Sun" && MonthGrid.weekdayLabels(sunday).last == "Sat",
               "headings from Sunday")
        expect(MonthGrid.weekdayLabels(cal) == ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"],
               "headings from Monday")
        let sat = calendar(firstWeekday: 7)
        // Saturday first: October 1 is a Thursday, five days after Saturday.
        expect(MonthGrid.gridDays(month: month("2026-10-15"), calendar: sat).first == day("2026-09-26", sat),
               "Saturday first starts October on Sep 26")
    }

    /// A day added by seconds lands an hour off across a clock change, and a
    /// 23-hour Sunday drifts every later cell. Every cell is a local midnight
    /// and consecutive cells are consecutive dates.
    static func gridDaysAreMidnightsAcrossDST() {
        for m in ["2026-03-10", "2026-11-10"] {
            let days = MonthGrid.gridDays(month: month(m), calendar: cal)
            let midnights = days.allSatisfy { cal.dateComponents([.hour, .minute], from: $0) == DateComponents(hour: 0, minute: 0) }
            expect(midnights, "every cell of \(m)'s grid is a local midnight")
            let steps = zip(days, days.dropFirst()).allSatisfy {
                cal.dateComponents([.day], from: $0, to: $1).day == 1
            }
            expect(steps, "\(m)'s cells are one date apart")
            expect(Set(days).count == days.count, "\(m) has no repeated day")
        }
    }

    // MARK: the strip

    /// 7:00am exactly starts the waking strip and leaves the night cap empty;
    /// something over at exactly 7:00am is all night cap; 5 to 9am is both,
    /// flat where it crosses.
    static func sevenAmIsTheStripsEdge() {
        let d = day("2026-10-07")
        let model = items([
            ghost(1, "Run", start: "2026-10-07T07:00:00-07:00", end: "2026-10-07T08:00:00-07:00",
                  commitment: .committed),
            ghost(2, "Early", start: "2026-10-07T06:00:00-07:00", end: "2026-10-07T07:00:00-07:00",
                  commitment: .committed),
            ghost(3, "Flight", start: "2026-10-07T05:00:00-07:00", end: "2026-10-07T09:00:00-07:00",
                  commitment: .committed),
        ])
        let run = MonthGrid.segments(model.first { $0.title == "Run" }!, on: d, calendar: cal)
        expect(run.count == 1 && run[0].part == .day, "7:00am is on the strip, not in the night cap")
        expect(run.first.map { near($0.lo, 0) && near($0.hi, 1 / 17) && !$0.continuesBefore } == true,
               "at its very start, rounded")
        let early = MonthGrid.segments(model.first { $0.title == "Early" }!, on: d, calendar: cal)
        expect(early.count == 1 && early[0].part == .night, "over at 7:00am is night cap only")
        expect(early.first.map { near($0.lo, 6 / 7) && near($0.hi, 1) && !$0.continuesAfter } == true,
               "filling its last seventh, with a rounded end")
        let flight = MonthGrid.segments(model.first { $0.title == "Flight" }!, on: d, calendar: cal)
        expect(flight.count == 2, "5 to 9am is in both parts")
        expect(flight.first.map { $0.part == .night && near($0.lo, 5 / 7) && $0.continuesAfter } == true,
               "the night cap's piece runs on")
        expect(flight.last.map { $0.part == .day && near($0.hi, 2 / 17) && $0.continuesBefore } == true,
               "the strip's piece came from it")
    }

    /// The mock's Night Shift: 10pm Saturday to 6am Sunday leaves Saturday
    /// flat and lands in Sunday's night cap flat. And on the last Saturday of
    /// October, the party crosses into the greyed November 1, which the grid
    /// still draws.
    static func aPartyRunsOffSaturdayIntoSundaysNightCap() {
        let model = build([
            ghost(1, "Night Shift", start: "2026-10-10T22:00:00-07:00", end: "2026-10-11T06:00:00-07:00",
                  commitment: .committed),
            ghost(2, "Halloween", start: "2026-10-31T21:00:00-07:00", end: "2026-11-01T02:00:00-08:00",
                  band: .likely),
            ghost(3, "Late dinner", start: "2026-10-12T21:00:00-07:00", end: "2026-10-13T00:00:00-07:00",
                  commitment: .committed),
        ])
        let sat = cell(model, "2026-10-10")?.segments ?? []
        expect(sat.count == 1, "Saturday draws the party once")
        expect(sat.first.map { $0.part == .day && near($0.lo, 15 / 17) && near($0.hi, 1) && $0.continuesAfter } == true,
               "from 10pm to the end of Saturday's strip, flat")
        let sun = cell(model, "2026-10-11")?.segments ?? []
        expect(sun.count == 1, "Sunday draws it too")
        expect(sun.first.map { $0.part == .night && near($0.lo, 0) && near($0.hi, 6 / 7) && $0.continuesBefore } == true,
               "in its night cap, from midnight to 6am, flat")
        let nov1 = cell(model, "2026-11-01")
        expect(nov1?.inMonth == false && nov1?.segments.first?.part == .night,
               "Halloween runs into the greyed November 1")
        expect(nov1?.segments.first.map { near($0.hi, 2 / 7) } == true,
               "to 2am, read on the wall clock")
        expect(cell(model, "2026-10-13")?.segments.isEmpty == true,
               "ending exactly at midnight touches nothing of the next day")
        expect(cell(model, "2026-10-12")?.segments.first.map { !$0.continuesAfter && near($0.hi, 1) } == true,
               "and keeps its rounded end")
    }

    /// The Sundays the clocks change: 9am is where 9am always is, a party over
    /// the fall-back night ends at 6am on the cap, and the hour that happens
    /// twice still draws something.
    static func theAxisIsWallTimeOnDSTWeekends() {
        for (d, offset) in [("2026-03-08", "-07:00"), ("2026-11-01", "-08:00")] {
            let i = items([ghost(1, "Brunch", start: "\(d)T09:00:00\(offset)", end: "\(d)T10:00:00\(offset)",
                                 commitment: .committed)])
            let s = MonthGrid.segments(i[0], on: day(d), calendar: cal)
            expect(s.count == 1 && near(s[0].lo, 2 / 17) && near(s[0].hi, 3 / 17),
                   "9 to 10am on \(d) sits at 9 to 10am")
        }
        let party = items([ghost(2, "Party", start: "2026-10-31T22:00:00-07:00",
                                 end: "2026-11-01T06:00:00-08:00", commitment: .committed)])
        let cap = MonthGrid.segments(party[0], on: day("2026-11-01"), calendar: cal)
        expect(cap.count == 1 && near(cap[0].hi, 6 / 7), "a 25-hour night still ends at 6am on the cap")
        let spring = items([ghost(4, "Party", start: "2026-03-07T22:00:00-08:00",
                                  end: "2026-03-08T06:00:00-07:00", commitment: .committed)])
        let short = MonthGrid.segments(spring[0], on: day("2026-03-08"), calendar: cal)
        expect(short.count == 1 && near(short[0].hi, 6 / 7), "and a 23-hour one too")
        // 1:30am PDT to 1:15am PST: 45 minutes that read backwards on the wall.
        let twice = items([ghost(3, "Twice", start: "2026-11-01T01:30:00-07:00",
                                 end: "2026-11-01T01:15:00-08:00", commitment: .committed)])
        let back = MonthGrid.segments(twice[0], on: day("2026-11-01"), calendar: cal)
        expect(back.count == 1 && back[0].hi > back[0].lo, "the repeated hour still has width")
    }

    // MARK: spans

    /// Lisbon, Oct 14 to 19 (a ghost's date-only end names its last day): a
    /// bar from Wednesday to the end of that row, then Monday alone on the
    /// next. A three-week stay crosses four rows and is named on each.
    static func spansAcrossWeekRows() {
        let model = build([
            ghost(1, "Lisbon", start: "2026-10-14", end: "2026-10-19", allDay: true, commitment: .committed),
            ghost(3, "Flight to Lisbon", start: "2026-10-14T18:05:00-07:00", end: "2026-10-16T08:00:00+01:00",
                  commitment: .committed),
        ])
        // Alone in its month: with Lisbon and the flight it would be a third
        // lane, and counted rather than drawn (spanLanesAndOverflow).
        let stay = build([
            ghost(2, "Sabbatical", start: "2026-10-03", end: "2026-10-20", allDay: true, commitment: .committed)
        ])
        func bars(_ title: String) -> [(row: Int, bar: SpanSegment)] {
            (title == "Sabbatical" ? stay : model).weeks.enumerated().flatMap { r, week in
                week.spans.filter { $0.item.title == title }.map { (r, $0) }
            }
        }
        let lisbon = bars("Lisbon")
        expect(lisbon.count == 2, "Lisbon crosses two rows")
        expect(lisbon.first.map { $0.row == 2 && $0.bar.firstColumn == 2 && $0.bar.lastColumn == 6 } == true,
               "Wednesday to Sunday of the 12th's row")
        expect(lisbon.first.map { $0.bar.startsHere && !$0.bar.endsHere } == true,
               "rounded where it begins, flat where it runs on")
        expect(lisbon.last.map { $0.row == 3 && $0.bar.firstColumn == 0 && $0.bar.lastColumn == 0 } == true,
               "and Monday the 19th alone on the next")
        expect(lisbon.last.map { !$0.bar.startsHere && $0.bar.endsHere } == true,
               "flat where it came from, rounded at its end")
        let long = bars("Sabbatical")
        expect(long.map(\.row) == [0, 1, 2, 3], "a multi-week span is a bar on every row it touches")
        expect(long.first.map { $0.bar.firstColumn == 5 && $0.bar.lastColumn == 6 } == true,
               "from Saturday the 3rd")
        expect(long.dropFirst().dropLast().allSatisfy { $0.bar.firstColumn == 0 && $0.bar.lastColumn == 6 },
               "whole rows in the middle")
        expect(long.last.map { $0.bar.lastColumn == 1 } == true, "to Tuesday the 20th")
        expect(long.allSatisfy { MonthGrid.spanLabel($0.bar, calendar: cal).title == "Sabbatical" },
               "named on every row, not only the first")
        let flight = bars("Flight to Lisbon")
        expect(flight.first.map { MonthGrid.spanLabel($0.bar, calendar: cal).sub == "6:05pm" } == true,
               "a timed span says when it leaves where it starts")
        expect(lisbon.allSatisfy { MonthGrid.spanLabel($0.bar, calendar: cal).sub == nil },
               "an all-day one says nothing more")
        expect(cell(model, "2026-10-15")?.segments.isEmpty == true,
               "a span puts no ink on the strips it crosses")
    }

    static func whatIsAndIsNotASpan() {
        let i = items([
            ghost(1, "Overnight", start: "2026-10-10T22:00:00-07:00", end: "2026-10-11T06:00:00-07:00",
                  commitment: .committed),
            ghost(2, "Hackathon", start: "2026-10-10T09:00:00-07:00", end: "2026-10-11T09:00:00-07:00",
                  commitment: .committed),
            ghost(3, "Rent", start: "2026-10-05", allDay: true, commitment: .committed),
        ], [event("e1", "Offsite", start: "2026-10-21", end: "2026-10-22", allDay: true)])
        func span(_ t: String) -> Bool { MonthGrid.isSpan(i.first { $0.title == t }!, calendar: cal) }
        expect(!span("Overnight"), "an overnight party is ink, not a span")
        expect(span("Hackathon"), "a timed 24 hours is a span")
        expect(!span("Rent"), "an all-day single day is not a span")
        expect(span("Offsite"), "a real two-day all-day event is a span")
    }

    /// Rent due: a faint bar under that day's strip, no block, no span.
    static func anAllDaySingleDayIsABarNotABlock() {
        let model = build([ghost(1, "Rent due", start: "2026-10-05", allDay: true, commitment: .committed)])
        let rent = cell(model, "2026-10-05")
        expect(rent?.allDay.map(\.title) == ["Rent due"], "it is the day's all-day bar")
        expect(rent?.segments.isEmpty == true, "and nothing on the strip")
        expect(model.weeks.allSatisfy(\.spans.isEmpty), "and no span")
        expect(cell(model, "2026-10-06")?.allDay.isEmpty == true, "only on its own day")
    }

    /// Two trips that overlap take two lanes; a third is counted in the
    /// cells it would have covered; one that starts after the first ends
    /// takes the first lane back.
    static func spanLanesAndOverflow() {
        let model = build([
            ghost(1, "Tahoe", start: "2026-10-19", end: "2026-10-22", allDay: true, commitment: .committed),
            ghost(2, "Conference", start: "2026-10-20", end: "2026-10-21", allDay: true, commitment: .committed),
            ghost(3, "Guests", start: "2026-10-21", end: "2026-10-23", allDay: true, commitment: .committed),
            ghost(4, "Camping", start: "2026-10-24", end: "2026-10-25", allDay: true, commitment: .committed),
        ])
        let row = model.weeks[3]
        func lane(_ t: String) -> Int? { row.spans.first { $0.item.title == t }?.lane }
        expect(lane("Tahoe") == 0, "the earliest takes the first lane")
        expect(lane("Conference") == 1, "the overlapping one the second")
        expect(lane("Guests") == nil, "a third at once has no lane")
        expect(row.overflow == [0, 0, 1, 1, 1, 0, 0], "and is counted on the 21st to the 23rd")
        expect(lane("Camping") == 0, "a later trip takes the first lane back")
        expect(model.lanes == 2, "the month reserves two lanes")
        let quiet = build([ghost(1, "Lisbon", start: "2026-10-14", end: "2026-10-19", allDay: true,
                                 commitment: .committed)])
        expect(quiet.lanes == 1, "a month with one trip reserves one")
        expect(build([]).lanes == 0, "and one with none reserves none")
    }

    /// Unlikely ghosts are never drawn; each day says how many start there.
    static func unlikelyIsCountedNotDrawn() {
        let model = build([
            ghost(1, "Webinar", start: "2026-10-08T12:00:00-07:00", end: "2026-10-08T13:00:00-07:00", band: .unlikely),
            ghost(2, "Meetup", start: "2026-10-08T18:30:00-07:00", band: .unlikely),
            ghost(3, "Dinner", start: "2026-10-08T19:00:00-07:00", end: "2026-10-08T22:00:00-07:00", band: .unlikely),
            ghost(4, "Retreat", start: "2026-10-15", end: "2026-10-18", allDay: true, band: .unlikely),
            ghost(5, "Drinks", start: "2026-10-08T18:00:00-07:00", end: "2026-10-08T19:30:00-07:00", band: .likely),
        ])
        let thu = cell(model, "2026-10-08")
        expect(thu?.unlikely.count == 3, "three unlikely on Thursday")
        expect(thu?.segments.map(\.item.title) == ["Drinks"], "only the likely one is ink")
        expect(cell(model, "2026-10-15")?.unlikely.map(\.title) == ["Retreat"],
               "an unlikely trip is counted on the day it starts")
        expect(cell(model, "2026-10-16")?.unlikely.isEmpty == true, "and nowhere else")
        expect(model.weeks.allSatisfy(\.spans.isEmpty), "and is no span")
    }

    // MARK: the column

    static func theDayColumn() {
        let i = items([
            ghost(1, "Night Shift", start: "2026-10-10T22:00:00-07:00", end: "2026-10-11T06:00:00-07:00",
                  commitment: .committed),
            ghost(2, "Brunch", start: "2026-10-11T11:00:00-07:00", end: "2026-10-11T12:30:00-07:00", band: .maybe),
            ghost(3, "Picnic", start: "2026-10-11T13:00:00-07:00", end: "2026-10-11T16:00:00-07:00", band: .likely),
            ghost(4, "Gala", start: "2026-10-11T19:00:00-07:00", band: .unlikely),
            ghost(5, "Lisbon", start: "2026-10-09", end: "2026-10-12", allDay: true, commitment: .committed),
            ghost(6, "Bill", start: "2026-10-11", allDay: true, commitment: .committed),
        ])
        let col = MonthGrid.column(i, day: day("2026-10-11"), calendar: cal)
        expect(col.night.map(\.segment.item.title) == ["Night Shift"], "last night's tail is in the night row")
        expect(col.night.first?.segment.continuesBefore == true, "flat-topped")
        expect(col.blocks.map(\.segment.item.title) == ["Brunch", "Picnic"], "the day's blocks, by start")
        expect(col.allDay.map(\.title) == ["Lisbon", "Bill"], "the span and the all-day item are words")
        expect(col.unlikely.map(\.title) == ["Gala"], "the unlikely one folds")
        expect(col.order.map(\.title) == ["Lisbon", "Bill", "Night Shift", "Brunch", "Picnic"],
               "j and k go words first, then blocks by start")
        let sat = MonthGrid.column(i, day: day("2026-10-10"), calendar: cal)
        expect(sat.blocks.first.map { $0.segment.continuesAfter && near($0.segment.lo, 15 / 17) } == true,
               "the night it began, the party runs off the bottom")
    }

    static func columnLanes() {
        let i = items([
            ghost(1, "A", start: "2026-10-08T09:00:00-07:00", end: "2026-10-08T10:00:00-07:00", commitment: .committed),
            ghost(2, "B", start: "2026-10-08T09:30:00-07:00", end: "2026-10-08T10:30:00-07:00", commitment: .committed),
            ghost(3, "C", start: "2026-10-08T10:00:00-07:00", end: "2026-10-08T10:30:00-07:00", commitment: .committed),
            ghost(5, "E", start: "2026-10-08T10:30:00-07:00", end: "2026-10-08T11:00:00-07:00", commitment: .committed),
            ghost(4, "D", start: "2026-10-08T14:00:00-07:00", end: "2026-10-08T15:00:00-07:00", commitment: .committed),
        ])
        let col = MonthGrid.column(i, day: day("2026-10-08"), calendar: cal)
        func block(_ t: String) -> ColumnBlock? { col.blocks.first { $0.segment.item.title == t } }
        expect(block("A")?.lane == 0 && block("B")?.lane == 1, "two at once, side by side")
        expect(block("C")?.lane == 0, "C takes A's lane back the moment A ends")
        expect(block("A")?.lanes == 2 && block("C")?.lanes == 2, "the run shares two lanes")
        expect(block("E")?.lane == 0 && block("E")?.lanes == 1,
               "starting the moment the overlap ends, E is full width again")
        expect(block("D")?.lane == 0 && block("D")?.lanes == 1, "an afternoon alone is full width")
    }

    static func columnTimes() {
        let i = items([
            ghost(1, "Sync", start: "2026-10-08T10:00:00-07:00", end: "2026-10-08T11:00:00-07:00", commitment: .committed),
            ghost(2, "Lunch", start: "2026-10-08T11:00:00-07:00", end: "2026-10-08T12:30:00-07:00", commitment: .committed),
            ghost(3, "Show", start: "2026-10-08T20:00:00-07:00", end: "2026-10-08T23:00:00-07:00", estimated: true,
                  commitment: .committed),
            ghost(4, "Party", start: "2026-10-10T22:00:00-07:00", end: "2026-10-11T06:00:00-07:00", commitment: .committed),
        ])
        func text(_ t: String, _ d: String) -> String {
            MonthGrid.rangeText(i.first { $0.title == t }!, on: day(d), calendar: cal)
        }
        expect(text("Sync", "2026-10-08") == "10 to 11am", "one meridiem when both share it")
        expect(text("Lunch", "2026-10-08") == "11am to 12:30pm", "both when they differ")
        expect(text("Show", "2026-10-08") == "8pm to ~11pm", "a guessed end keeps its tilde and its pm")
        expect(text("Party", "2026-10-10") == "10pm to 6am Sun", "past midnight says which day")
        expect(text("Party", "2026-10-11") == "until 6am", "the morning after says until")
    }

    // MARK: moving

    static func arrowsAndMonths() {
        let oct = MonthPlace(month: month("2026-10-15"), day: day("2026-10-01"))
        func go(_ p: MonthPlace, _ m: MonthMove) -> MonthPlace {
            MonthGrid.navigate(p, m, today: now, calendar: cal)
        }
        let left = go(oct, .days(-1))
        expect(left.day == day("2026-09-30") && left.month == oct.month,
               "left from Oct 1 lands on the greyed Sep 30 and the page stays")
        let up = go(oct, .days(-7))
        expect(up.day == day("2026-09-24") && up.month == month("2026-09-15"),
               "up off the grid turns to September")
        let last = MonthPlace(month: oct.month, day: day("2026-10-31"))
        let nov1 = go(last, .days(1))
        expect(nov1.day == day("2026-11-01") && nov1.month == oct.month, "Nov 1 is still October's last cell")
        let nov2 = go(nov1, .days(1))
        expect(nov2.day == day("2026-11-02") && nov2.month == month("2026-11-15"), "Nov 2 turns the page")
        let down = go(MonthPlace(month: oct.month, day: day("2026-10-26")), .days(7))
        expect(down.day == day("2026-11-02") && down.month == month("2026-11-15"), "down off the last row too")
        expect(go(last, .months(1)).day == day("2026-11-30"), "the 31st becomes November's 30th")
        let jan31 = MonthPlace(month: month("2027-01-15"), day: day("2027-01-31"))
        expect(go(jan31, .months(1)).day == day("2027-02-28"), "and February's 28th")
        expect(go(jan31, .months(-1)) == MonthPlace(month: month("2026-12-15"), day: day("2026-12-31")),
               "back a month keeps the 31st where December has one")
        let greyed = MonthPlace(month: oct.month, day: day("2026-09-29"))
        expect(go(greyed, .months(1)).day == day("2026-11-01"), "from a greyed day a month lands on the 1st")
        expect(go(greyed, .today) == MonthPlace(month: oct.month, day: day("2026-10-05")), "t is today")
        expect(MonthGrid.select(day("2026-11-01"), in: oct, calendar: cal).month == oct.month,
               "clicking a greyed day keeps the month")
        let year = go(MonthPlace(month: month("2026-12-15"), day: day("2026-12-10")), .months(1))
        expect(year.month == month("2027-01-15") && year.day == day("2027-01-10"), "across the year")
    }

    // MARK: the request

    static func theRequestedRange() {
        let (from, to) = MonthGrid.requestRange(month: month("2026-10-15"), now: now, calendar: cal)
        expect(from == day("2026-08-28"), "this month reaches back a month before Sep 28 for spans")
        expect(to == day("2027-01-03"), "and keeps the agenda's 90 days ahead of yesterday")
        let (from2, to2) = MonthGrid.requestRange(month: month("2026-12-15"), now: now, calendar: cal)
        expect(from2 == day("2026-10-04") && to2 == day("2027-01-04"),
               "December: from yesterday (the agenda) to the grid's end")
        let (farFrom, farTo) = MonthGrid.requestRange(month: month("2028-10-15"), now: now, calendar: cal)
        let grid = MonthGrid.gridBounds(month: month("2028-10-15"), calendar: cal)
        expect(farFrom == MonthGrid.addDays(-31, to: grid.start, cal) && farTo == grid.end,
               "two years out, the month alone")
        expect(farTo.timeIntervalSince(farFrom) <= 400 * 86_400, "inside the daemon's clamp")
        for m in ["2025-01-15", "2026-06-15", "2027-10-15"] {
            let (f, t) = MonthGrid.requestRange(month: month(m), now: now, calendar: cal)
            let g = MonthGrid.gridBounds(month: month(m), calendar: cal)
            expect(f <= g.start && t >= g.end && t.timeIntervalSince(f) <= 400 * 86_400,
                   "\(m)'s range covers its grid and fits the clamp")
        }
    }

    static func nowOnTheStrip() {
        let mark = MonthGrid.nowMark(now, day: day("2026-10-05"), calendar: cal)
        expect(mark.map { $0.part == .day && near($0.at, (8 + 40.0 / 60) / 17) } == true, "3:40pm on the strip")
        let early = MonthGrid.nowMark(at("2026-10-05T06:00:00-07:00"), day: day("2026-10-05"), calendar: cal)
        expect(early.map { $0.part == .night && near($0.at, 6 / 7) } == true, "6am in the night cap")
        expect(MonthGrid.nowMark(now, day: day("2026-10-06"), calendar: cal) == nil, "only on today")
    }

    static func expect(_ cond: Bool, _ what: String) {
        checks += 1
        if !cond {
            failures += 1
            print("  FAIL: \(what)")
        }
    }
}
