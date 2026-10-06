// The month view's arithmetic, against fixtures (Lib/MonthGrid.swift), and
// the calendar's colours (Lib/CalendarColor.swift).
//
// Each of these is wrong only on somebody's screen in a particular month: the
// grid a row short in a month that starts on a Sunday, the party listed on
// both nights it touches, every block in the day column a sliver to the left
// on the Sunday the clocks change, the trip that loses its name on the second
// row it crosses, the cell that folds its last row into "+1 more" when the
// row would have fitted, a Banana calendar whose time is unreadable on its own
// chip. The fixture month is the mock's
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
        aCellListsTopDown()
        theCellFold()
        cellsFollowTheContainersWidth()
        nowInTodaysList()
        theMonthComesFirst()
        colourResolution()
        coloursSitInsideThePalette()

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

    /// The mock's Night Shift: 10pm Saturday to 6am Sunday is a row on
    /// Saturday, the night it begins, and not on Sunday (the day column
    /// shows its tail). Halloween on the last Saturday of October is on the
    /// 31st, not the greyed November 1 it runs into.
    static func aPartyRunsOffSaturdayIntoSundaysNightCap() {
        let model = build([
            ghost(1, "Night Shift", start: "2026-10-10T22:00:00-07:00", end: "2026-10-11T06:00:00-07:00",
                  commitment: .committed),
            ghost(2, "Halloween", start: "2026-10-31T21:00:00-07:00", end: "2026-11-01T02:00:00-08:00",
                  band: .likely),
            ghost(3, "Late dinner", start: "2026-10-12T21:00:00-07:00", end: "2026-10-13T00:00:00-07:00",
                  commitment: .committed),
        ])
        expect(cell(model, "2026-10-10")?.items.map(\.title) == ["Night Shift"],
               "Saturday lists the party, the night it begins")
        expect(cell(model, "2026-10-11")?.items.isEmpty == true, "Sunday does not list it again")
        expect(cell(model, "2026-10-31")?.items.map(\.title) == ["Halloween"], "Halloween is on the 31st")
        let nov1 = cell(model, "2026-11-01")
        expect(nov1?.inMonth == false && nov1?.items.isEmpty == true,
               "and not on the greyed November 1 it runs into")
        expect(cell(model, "2026-10-12")?.items.map(\.title) == ["Late dinner"], "a late dinner is its own day's")
        expect(cell(model, "2026-10-13")?.items.isEmpty == true,
               "ending exactly at midnight touches nothing of the next day")
        // The column, not the grid, still draws the morning after.
        let sun = MonthGrid.segments(model.days.flatMap(\.items).first { $0.title == "Night Shift" }!,
                                     on: day("2026-10-11"), calendar: cal)
        expect(sun.first.map { $0.part == .night && near($0.hi, 6 / 7) && $0.continuesBefore } == true,
               "Sunday's column has the tail, midnight to 6am")
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
        expect(cell(model, "2026-10-15")?.items.isEmpty == true,
               "a span is not also a row in the cells it crosses")
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

    /// Rent due: a row in its own cell, not a span.
    static func anAllDaySingleDayIsABarNotABlock() {
        let model = build([ghost(1, "Rent due", start: "2026-10-05", allDay: true, commitment: .committed)])
        let rent = cell(model, "2026-10-05")
        expect(rent?.items.map(\.title) == ["Rent due"] && rent?.items.first?.allDay == true,
               "it is the day's all-day row")
        expect(model.weeks.allSatisfy(\.spans.isEmpty), "and no span")
        expect(cell(model, "2026-10-06")?.items.isEmpty == true, "only on its own day")
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
        expect(row.lanes == 2, "that row keeps two lanes")
        expect(model.weeks[0].lanes == 0, "a row with no trips keeps none, and its cells start under the date")
        let quiet = build([ghost(1, "Lisbon", start: "2026-10-14", end: "2026-10-19", allDay: true,
                                 commitment: .committed)])
        expect(quiet.weeks.map(\.lanes) == [0, 0, 1, 1, 0], "one trip keeps one lane in the two rows it crosses")
        expect(build([]).weeks.allSatisfy { $0.lanes == 0 }, "and a month with none keeps none")
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
        expect(thu?.items.map(\.title) == ["Drinks"], "only the likely one is a row")
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

    // MARK: the cells

    static func colouredEvent(
        _ id: String, _ title: String, start: String, end: String? = nil, allDay: Bool = false,
        color: String? = "#039be5", calendar: String = "primary"
    ) -> TimelineEvent {
        TimelineEvent(
            id: id, calendar_id: calendar, calendar_name: "Work", title: title, start: start,
            end: end, all_day: allDay, start_timezone: nil, location: nil, status: .confirmed,
            self_response: nil, busy: true, organizer: nil, html_link: nil, color: color,
            editable: true, description: nil)
    }

    /// Thursday Oct 8: the bill first (all day), then the timed rows by
    /// start, ghosts and real events together. "7:45pm  Dreamers Dinner".
    static func aCellListsTopDown() {
        let model = build(
            [
                ghost(1, "Standup", start: "2026-10-08T09:30:00-07:00", end: "2026-10-08T09:45:00-07:00",
                      commitment: .committed),
                ghost(2, "Electric bill", start: "2026-10-08", allDay: true, commitment: .committed),
            ],
            [colouredEvent("d", "Dreamers Dinner", start: "2026-10-08T19:45:00-07:00",
                           end: "2026-10-08T21:30:00-07:00")])
        let thu = cell(model, "2026-10-08")?.items ?? []
        expect(thu.map(\.title) == ["Electric bill", "Standup", "Dreamers Dinner"],
               "all-day first, then by start")
        let dinner = thu[2]
        expect(MonthGrid.cellTime(dinner, cellWidth: 130, calendar: cal) == "7:45pm", "a timed row leads with its time")
        expect(MonthGrid.cellTime(thu[1], cellWidth: 130, calendar: cal) == "9:30am", "a ghost's too")
        expect(MonthGrid.cellTime(thu[0], cellWidth: 130, calendar: cal) == nil, "an all-day row has none")
        expect(MonthGrid.cellTime(dinner, cellWidth: 80, calendar: cal) == nil,
               "a narrow cell keeps the title and drops the time")
        expect(MonthGrid.cellTime(dinner, cellWidth: MonthGrid.rowTimeMinWidth, calendar: cal) != nil,
               "at exactly the threshold the time stays")
    }

    /// What fits and what folds. The last row is "+N more" only when
    /// something is left over; a cell that holds everything shows everything.
    static func theCellFold() {
        func fold(_ items: Int, _ spans: Int, _ slots: Int) -> CellFold {
            MonthGrid.fold(items: items, hiddenSpans: spans, slots: slots)
        }
        expect(fold(3, 0, 5) == CellFold(shown: 3, more: 0), "three in five: all shown")
        expect(fold(5, 0, 5) == CellFold(shown: 5, more: 0), "exactly full: all shown, no fold")
        expect(fold(6, 0, 5) == CellFold(shown: 4, more: 2), "one over: four shown and +2 more")
        expect(fold(2, 1, 5) == CellFold(shown: 2, more: 1), "a trip with no lane is counted in the fold")
        expect(fold(0, 2, 3) == CellFold(shown: 0, more: 2), "only lane-less trips: +2 more")
        expect(fold(4, 0, 1) == CellFold(shown: 0, more: 4), "one slot is the fold itself")
        expect(fold(4, 0, 0) == CellFold(shown: 0, more: 4), "no room shows nothing and still counts")
        expect(fold(0, 0, 0) == CellFold(shown: 0, more: 0), "an empty day is empty")

        func slots(_ h: Double, lanes: Int = 0) -> Int {
            MonthGrid.slots(cellHeight: h, lanes: lanes, numberBand: 26, rowHeight: 17, rowGap: 2, foot: 4)
        }
        // 140pt (five rows down an 800pt window): 110 left, five 17pt rows
        // and their gaps take 93, a sixth would need 112.
        expect(slots(140) == 5, "a 140pt cell holds five rows")
        expect(slots(140, lanes: 2) == 3, "two trip lanes take two of them")
        expect(slots(121) == 4 && slots(123) == 5, "a row fits the moment its height does, gap and all")
        expect(slots(30) == 0, "a sliver of a cell holds none")
        expect(slots(200) > slots(140), "a taller cell holds more")
    }

    /// The founder's bug: widen the window, the grid widens; narrow it, it
    /// stayed wide. The cell width is a pure seventh of the container's
    /// width, so it has no memory of the width before.
    static func cellsFollowTheContainersWidth() {
        let wide = MonthGrid.cellWidth(gridWidth: 980)
        let narrow = MonthGrid.cellWidth(gridWidth: 560)
        expect(near(wide, 140) && near(narrow, 80), "a seventh of the grid")
        expect(narrow < wide, "a narrower window gives narrower cells")
        _ = MonthGrid.cellWidth(gridWidth: 1400)
        expect(near(MonthGrid.cellWidth(gridWidth: 560), 80), "and back again after a wider one, no ratchet")
        expect(MonthGrid.cellWidth(gridWidth: -10) == 0, "never negative")
        let dinner = items([], [colouredEvent("d", "Dinner", start: "2026-10-08T19:45:00-07:00")])[0]
        expect(MonthGrid.cellTime(dinner, cellWidth: wide, calendar: cal) != nil
               && MonthGrid.cellTime(dinner, cellWidth: narrow, calendar: cal) == nil,
               "and the narrower cell drops the time the wider one shows")
    }

    static func nowInTodaysList() {
        let i = items([
            ghost(1, "Bill", start: "2026-10-05", allDay: true, commitment: .committed),
            ghost(2, "Lunch", start: "2026-10-05T12:00:00-07:00", end: "2026-10-05T13:00:00-07:00",
                  commitment: .committed),
            ghost(3, "Gym", start: "2026-10-05T17:00:00-07:00", end: "2026-10-05T18:00:00-07:00",
                  commitment: .committed),
        ]).sorted(by: Agenda.precedes)
        expect(MonthGrid.nowSlot(i, now: now) == 2, "at 3:40pm the line sits above the gym")
        expect(MonthGrid.nowSlot(i, now: at("2026-10-05T08:00:00-07:00")) == 1,
               "in the morning, under the bill and above lunch")
        expect(MonthGrid.nowSlot(i, now: at("2026-10-05T21:00:00-07:00")) == 3, "at night, under everything")
        expect(MonthGrid.nowSlot(Array(i.prefix(1)), now: now) == nil, "no line among all-day rows alone")
    }

    static func theMonthComesFirst() {
        expect(CalendarViewMode.initial == .month, "the page opens on the month")
        expect(CalendarViewMode.pickerOrder == [.month, .agenda], "Month | Agenda, in that order")
    }

    // MARK: colour

    static func colourResolution() {
        expect(CalendarColor.parse("#039BE5") == RGB(hex: 0x039BE5), "Google's #rrggbb")
        expect(CalendarColor.parse(" 039be5 ") == RGB(hex: 0x039BE5), "bare and padded")
        expect(CalendarColor.parse("#abc") == RGB(hex: 0xAABBCC), "the short form")
        for bad in [nil, "", "blue", "#12345", "#1234567", "#gg0000"] as [String?] {
            expect(CalendarColor.parse(bad) == nil, "\(bad ?? "nil") is no colour")
        }
        let feed = TimelineFeed(
            ghosts: [
                ghost(1, "Booked", start: "2026-10-08T19:00:00-07:00", commitment: .committed),
                ghost(2, "Likely", start: "2026-10-08T19:00:00-07:00", band: .likely),
                ghost(3, "Maybe", start: "2026-10-08T19:00:00-07:00", band: .maybe),
                ghost(4, "Unlikely", start: "2026-10-08T19:00:00-07:00", band: .unlikely),
            ],
            events: [
                colouredEvent("a", "Coloured", start: "2026-10-08T09:00:00-07:00", color: "#f6bf26"),
                colouredEvent("b", "Old daemon", start: "2026-10-08T09:00:00-07:00", color: nil),
                colouredEvent("c", "Garbage", start: "2026-10-08T09:00:00-07:00", color: "tomato"),
            ])
        var cancelled = ghost(5, "Off", start: "2026-10-08T19:00:00-07:00", commitment: .committed)
        cancelled.mail_state = .cancelled
        let all = Agenda.items(feed, calendar: cal)
            + [Agenda.item(cancelled, calendar: cal)!, Agenda.item(feed.ghosts[3], calendar: cal)!]
        func style(_ t: String) -> ChipStyle { CalendarColor.style(all.first { $0.title == t }!) }
        expect(style("Coloured") == ChipStyle(hue: .calendar(RGB(hex: 0xF6BF26)), fill: .solid, thin: false, struck: false),
               "a real event wears its calendar's colour, solid")
        expect(style("Old daemon").hue == .plain, "no colour from an older daemon: the neutral")
        expect(style("Garbage").hue == .plain, "an unreadable colour: the neutral")
        expect(style("Booked") == ChipStyle(hue: .ghost, fill: .solid, thin: false, struck: false),
               "a committed ghost: the ghost hue, solid")
        expect(style("Likely").hue == .ghost && style("Likely").fill == .striped, "likely: striped")
        expect(style("Maybe").hue == .ghost && style("Maybe").fill == .outline, "maybe: an outline")
        expect(style("Off").struck && style("Off").thin && style("Off").hue == .ghost,
               "cancelled: struck through and thinned, in the ghost hue")
        expect(style("Unlikely").thin && !style("Unlikely").struck,
               "an unlikely ghost out of its fold is thinned, not struck")
        expect(CalendarColor.base(.ghost) == RGB(hex: 0x2B7FD4), "the ghost hue is the accent")
    }

    /// Google's calendar and event palettes, every one: each placed on the
    /// palette's own lightness for the theme, its hue kept, its chroma capped,
    /// its ink readable on its own fill, and the app's title ink readable on
    /// every fill. A grey calendar stays grey.
    static func coloursSitInsideThePalette() {
        let google: [UInt32] = [
            0xAC725E, 0xD06B64, 0xF83A22, 0xFA573C, 0xFF7537, 0xFFAD46, 0x42D692, 0x16A765,
            0x7BD148, 0xB3DC6C, 0xFBE983, 0xFAD165, 0x92E1C0, 0x9FE1E7, 0x9FC6E7, 0x4986E7,
            0x9A9CFF, 0xB99AFF, 0xC2C2C2, 0xCABDBF, 0xCCA6AC, 0xF691B2, 0xCD74E6, 0xA47AE2,
            0x7986CB, 0x33B679, 0x8E24AA, 0xE67C73, 0xF6BF26, 0xF4511E, 0x039BE5, 0x616161,
            0x3F51B5, 0x0B8043, 0xD50000, 0x2B7FD4, 0x8C7254,
        ]
        let titleLight = RGB(hex: 0x141C26)  // Palette.ink, light
        let titleDark = RGB(hex: 0xE9EEF5)  // Palette.ink, dark
        var fillLs: [Double] = []
        var worstInk = 99.0
        var worstTitle = 99.0
        var hueOK = true
        var capOK = true
        for hex in google {
            let base = RGB(hex: hex)
            let src = CalendarColor.oklch(base)
            for dark in [false, true] {
                let t = CalendarColor.tones(base, dark: dark)
                let fill = CalendarColor.oklch(t.fill)
                let ink = CalendarColor.oklch(t.ink)
                fillLs.append(fill.l - (dark ? CalendarColor.darkFill.l : CalendarColor.lightFill.l))
                worstInk = min(worstInk, RGB.contrast(t.ink, t.fill))
                worstTitle = min(worstTitle, RGB.contrast(dark ? titleDark : titleLight, t.fill))
                let cap = dark ? CalendarColor.darkFill.maxChroma : CalendarColor.lightFill.maxChroma
                if fill.c > cap + 0.006 { capOK = false }
                if src.c > 0.05, fill.c > 0.02 {
                    let d = abs(fill.h - src.h).truncatingRemainder(dividingBy: 360)
                    if min(d, 360 - d) > 6 { hueOK = false }
                }
                if src.c > 0.05, ink.c > 0.02 {
                    let d = abs(ink.h - src.h).truncatingRemainder(dividingBy: 360)
                    if min(d, 360 - d) > 8 { hueOK = false }
                }
            }
        }
        expect(fillLs.allSatisfy { abs($0) < 0.012 }, "every calendar's fill sits on the theme's one lightness")
        expect(hueOK, "and keeps its own hue")
        expect(capOK, "with its chroma capped")
        expect(worstInk >= 4.5, "the hue's ink reads on its own fill in both themes (worst \(worstInk))")
        expect(worstTitle >= 7, "the title ink reads on every fill in both themes (worst \(worstTitle))")
        let graphite = CalendarColor.tones(RGB(hex: 0x616161), dark: false)
        expect(CalendarColor.oklch(graphite.fill).c < 0.01, "a grey calendar stays grey")
        let banana = CalendarColor.oklch(CalendarColor.tones(RGB(hex: 0xF6BF26), dark: false).fill)
        let tomato = CalendarColor.oklch(CalendarColor.tones(RGB(hex: 0xD50000), dark: false).fill)
        expect(abs(banana.l - tomato.l) < 0.012 && abs(banana.h - tomato.h) > 30,
               "Banana and Tomato: the same weight, different hues")
        // The ghost is the accent's hue, and Peacock (#039be5) is a blue too:
        // the ghost's body must be the quieter of the two, its edge not.
        for dark in [false, true] {
            let ghostTones = CalendarColor.tones(for: .ghost, dark: dark)
            let peacockTones = CalendarColor.tones(for: .calendar(RGB(hex: 0x039BE5)), dark: dark)
            let ghostFill = CalendarColor.oklch(ghostTones.fill)
            let peacockFill = CalendarColor.oklch(peacockTones.fill)
            expect(ghostFill.c < peacockFill.c * 0.7, "a ghost's body is a quieter blue than Peacock's (dark: \(dark))")
            expect(CalendarColor.oklch(ghostTones.edge).c > ghostFill.c * 1.5,
                   "while its edge, which carries the band, keeps its strength (dark: \(dark))")
            expect(RGB.contrast(ghostTones.ink, ghostTones.fill) >= 4.5, "and its ink still reads (dark: \(dark))")
        }
        let light = CalendarColor.tones(RGB(hex: 0x039BE5), dark: false)
        let dark = CalendarColor.tones(RGB(hex: 0x039BE5), dark: true)
        expect(light.fill.luminance > light.ink.luminance && dark.fill.luminance < dark.ink.luminance,
               "light mode: dark ink on a light fill; dark mode the other way round")
    }

    static func expect(_ cond: Bool, _ what: String) {
        checks += 1
        if !cond {
            failures += 1
            print("  FAIL: \(what)")
        }
    }
}
