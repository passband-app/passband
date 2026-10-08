// The week view's arithmetic, against fixtures (Lib/WeekGrid.swift).
//
// Each of these is wrong only on somebody's screen in a particular week: the
// week that starts a day late for someone whose weeks start on Sunday, the
// Friday that is missing the week the clocks change, the trip that takes a
// different lane here than in the month above it, the all-day birthday drawn
// as a 17-hour block, three meetings at once drawn on top of each other, the
// party's morning-after tail missing from Sunday's night cap. The fixture is
// the month suite's: October 2026 in San Francisco, weeks from Monday.

import Foundation

@main
struct WeekGridTests {
    nonisolated(unsafe) static var failures = 0
    nonisolated(unsafe) static var checks = 0

    static let zone = TimeZone(identifier: "America/Los_Angeles")!
    static var cal: Calendar { calendar(firstWeekday: 2) }
    static func calendar(firstWeekday: Int) -> Calendar {
        var c = Calendar(identifier: .gregorian)
        c.timeZone = zone
        c.firstWeekday = firstWeekday
        return c
    }

    static func main() {
        whichWeek()
        sevenDaysAcrossDST()
        theWeekIsOneOfItsMonthsRows()
        overlappingEventsSitSideBySide()
        packingPerDayNotPerWeek()
        theStripTakesAllDayAndTrips()
        theStripIsTheMonthsLanes()
        stripOverflow()
        overnightAndUnlikely()
        theDraftIsOneOfTheBlocks()
        widthsAndLabels()
        titles()
        walking()
        thePicker()

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
            print("  FAIL: \(what)")
        }
    }

    // MARK: fixtures

    static func at(_ s: String) -> Date {
        AgendaTime.parse(s, zone: nil, calendar: cal)!.date
    }

    static func day(_ s: String, _ c: Calendar = cal) -> Date {
        c.startOfDay(for: at(s + "T12:00:00-07:00"))
    }

    static func ghost(
        _ id: Int, _ title: String, start: String, end: String? = nil, allDay: Bool = false,
        commitment: GhostCommitment = .committed, band: GhostBand? = nil
    ) -> TimelineGhost {
        TimelineGhost(
            id: id, kind: .invite, title: title, start: start,
            start_timezone: "America/Los_Angeles", all_day: allDay, end: end,
            end_estimated: false, end_source: end == nil ? nil : .stated,
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

    static func week(_ i: [AgendaItem], _ d: String = "2026-10-08", maxLanes: Int = WeekGrid.stripMaxLanes)
        -> WeekModel
    {
        WeekGrid.build(i, day: day(d), calendar: cal, maxLanes: maxLanes)
    }

    static func column(_ m: WeekModel, _ d: String) -> DayColumn? { m.columns.first { $0.day == day(d) } }
    static func block(_ m: WeekModel, _ d: String, _ title: String) -> ColumnBlock? {
        column(m, d)?.blocks.first { $0.segment.item.title == title }
    }
    static func bar(_ m: WeekModel, _ title: String) -> SpanSegment? { m.bars.first { $0.item.title == title } }

    // MARK: which week

    static func whichWeek() {
        // Thursday Oct 8 2026: Monday-first weeks start Oct 5, Sunday-first Oct 4.
        expect(WeekGrid.weekStart(day("2026-10-08"), calendar: cal) == day("2026-10-05"),
               "a Thursday's week starts on its Monday")
        expect(WeekGrid.weekStart(day("2026-10-05"), calendar: cal) == day("2026-10-05"),
               "the first day is its own week's start")
        expect(WeekGrid.weekStart(day("2026-10-11"), calendar: cal) == day("2026-10-05"),
               "Sunday is the last day of a Monday-first week")
        let sunday = calendar(firstWeekday: 1)
        expect(WeekGrid.weekStart(day("2026-10-08", sunday), calendar: sunday) == day("2026-10-04", sunday),
               "Sunday-first: the same Thursday's week starts Oct 4")
        expect(WeekGrid.weekStart(day("2026-10-11", sunday), calendar: sunday) == day("2026-10-11", sunday),
               "and Sunday Oct 11 starts the next one")
        // A time of day is its day.
        expect(WeekGrid.weekStart(at("2026-10-08T23:59:00-07:00"), calendar: cal) == day("2026-10-05"),
               "a late evening is still its own day's week")
        let m = week([])
        expect(m.start == day("2026-10-05") && m.columns.count == 7, "seven columns from the Monday")
        expect(m.days.last == day("2026-10-11"), "ending on the Sunday")
        expect(m.bars.isEmpty && m.lanes == 0 && WeekGrid.stripRows(m) == 0, "an empty week has no strip")
    }

    /// The weeks the clocks change are 167 and 169 hours: seven days all the
    /// same, each a local midnight.
    static func sevenDaysAcrossDST() {
        // Clocks go back Sunday Nov 1 2026; forward Sunday Mar 14 2027.
        for start in ["2026-10-26", "2027-03-08"] {
            let first = WeekGrid.weekStart(day(start), calendar: cal)
            let days = WeekGrid.days(from: first, calendar: cal)
            expect(days.count == 7, "\(start): seven days")
            expect(days.allSatisfy { cal.startOfDay(for: $0) == $0 }, "\(start): each a local midnight")
            expect(Set(days.map { cal.component(.day, from: $0) }).count == 7, "\(start): no day twice")
            expect(cal.component(.weekday, from: days[6]) == 1, "\(start): ending on its Sunday")
        }
        let fall = WeekGrid.days(from: day("2026-10-26"), calendar: cal)
        expect(fall[6] == day("2026-11-01") && fall[6].timeIntervalSince(fall[5]) == 86_400,
               "Nov 1 starts 24 hours after Oct 31, and is 25 hours long")
    }

    /// The page asks the daemon for the place's MONTH; the week it shows has
    /// to be inside that month's grid, or its edge days would come up empty.
    static func theWeekIsOneOfItsMonthsRows() {
        for first in [1, 2, 7] {
            let c = calendar(firstWeekday: first)
            for d in ["2026-10-01", "2026-10-31", "2026-09-28", "2026-11-01", "2027-01-01"] {
                let selected = day(d, c)
                // The month a place holds for this day: its own, or (a greyed
                // day picked by the pointer) the one whose grid it is in.
                let month = MonthGrid.monthStart(selected, calendar: c)
                let (g0, g1) = MonthGrid.gridBounds(month: month, calendar: c)
                let start = WeekGrid.weekStart(selected, calendar: c)
                expect(start >= g0 && MonthGrid.addDays(7, to: start, c) <= g1,
                       "\(d), weeks from \(first): the week is a row of its month's grid")
                expect(MonthGrid.gridDays(month: month, calendar: c).contains(start),
                       "\(d), weeks from \(first): and starts where a grid row does")
            }
        }
    }

    // MARK: packing

    /// Three at once are three lanes; the moment one ends its lane is free;
    /// touching is not overlapping.
    static func overlappingEventsSitSideBySide() {
        let m = week(items([
            ghost(1, "A", start: "2026-10-08T09:00:00-07:00", end: "2026-10-08T11:00:00-07:00"),
            ghost(2, "B", start: "2026-10-08T09:30:00-07:00", end: "2026-10-08T10:30:00-07:00"),
            ghost(3, "C", start: "2026-10-08T10:00:00-07:00", end: "2026-10-08T12:00:00-07:00"),
            ghost(4, "D", start: "2026-10-08T10:30:00-07:00", end: "2026-10-08T11:30:00-07:00"),
            ghost(5, "E", start: "2026-10-08T12:00:00-07:00", end: "2026-10-08T13:00:00-07:00"),
            ghost(6, "F", start: "2026-10-08T15:00:00-07:00", end: "2026-10-08T16:00:00-07:00"),
        ]))
        let d = "2026-10-08"
        expect(block(m, d, "A")?.lane == 0 && block(m, d, "B")?.lane == 1 && block(m, d, "C")?.lane == 2,
               "three at once take three lanes, in start order")
        expect(block(m, d, "D")?.lane == 1, "D starts as B ends and takes B's lane, the first free")
        expect([block(m, d, "A"), block(m, d, "B"), block(m, d, "C"), block(m, d, "D")].allSatisfy { $0?.lanes == 3 },
               "the whole run shares three lanes")
        expect(block(m, d, "E")?.lane == 0 && block(m, d, "E")?.lanes == 1,
               "E starts the moment the run ends: full width, not a fourth of it")
        expect(block(m, d, "F")?.lane == 0 && block(m, d, "F")?.lanes == 1, "an afternoon alone is full width")
        // No two blocks that overlap in time share a lane.
        let blocks = column(m, d)?.blocks ?? []
        var clash = false
        for a in blocks {
            for b in blocks where a.id != b.id && a.lane == b.lane {
                if a.segment.lo < b.segment.hi && b.segment.lo < a.segment.hi { clash = true }
            }
        }
        expect(!clash && blocks.count == 6, "no two overlapping blocks share a lane")
        expect(blocks.allSatisfy { $0.lane < $0.lanes }, "and every lane is inside its run's count")
    }

    /// Wednesday's pile-up does not narrow Thursday's lone meeting.
    static func packingPerDayNotPerWeek() {
        let m = week(items([
            ghost(1, "A", start: "2026-10-07T09:00:00-07:00", end: "2026-10-07T10:00:00-07:00"),
            ghost(2, "B", start: "2026-10-07T09:00:00-07:00", end: "2026-10-07T10:00:00-07:00"),
            ghost(3, "Solo", start: "2026-10-08T09:00:00-07:00", end: "2026-10-08T10:00:00-07:00"),
        ]))
        expect(block(m, "2026-10-07", "A")?.lanes == 2 && block(m, "2026-10-07", "B")?.lanes == 2,
               "Wednesday's two share the column")
        expect(block(m, "2026-10-07", "A")?.lane != block(m, "2026-10-07", "B")?.lane, "in different lanes")
        expect(block(m, "2026-10-08", "Solo")?.lanes == 1, "Thursday's is full width")
        expect(column(m, "2026-10-09")?.blocks.isEmpty == true, "and Friday is empty")
        expect(column(m, "2026-10-08")?.order.map(\.title) == ["Solo"], "j / k walk the day's own items")
    }

    // MARK: the strip

    static func theStripTakesAllDayAndTrips() {
        let i = items(
            [
                ghost(1, "Lisbon", start: "2026-10-06", end: "2026-10-09", allDay: true),
                ghost(2, "Standup", start: "2026-10-07T09:00:00-07:00", end: "2026-10-07T09:30:00-07:00"),
                // Timed, a day or longer: a stay, drawn as a bar.
                ghost(3, "Hotel", start: "2026-10-09T15:00:00-07:00", end: "2026-10-11T11:00:00-07:00"),
            ],
            [event("b", "Birthday", start: "2026-10-07", allDay: true)])
        let m = week(i)
        expect(Set(m.bars.map(\.item.title)) == ["Lisbon", "Birthday", "Hotel"],
               "the strip holds the trip, the all-day birthday and the timed stay")
        expect(bar(m, "Birthday").map { $0.firstColumn == 2 && $0.lastColumn == 2 } == true,
               "a single all-day item is a one-day bar on its Wednesday")
        expect(bar(m, "Birthday").map { $0.startsHere && $0.endsHere } == true, "rounded at both ends")
        expect(column(m, "2026-10-07")?.blocks.map(\.segment.item.title) == ["Standup"],
               "and never a block on the hours: only the standup is")
        expect(column(m, "2026-10-07")?.allDay.map(\.title).contains("Birthday") == true,
               "the day still counts it among its all-day items")
        expect(bar(m, "Hotel").map { $0.firstColumn == 4 && $0.lastColumn == 6 } == true,
               "the stay runs Friday to Sunday")
        expect(column(m, "2026-10-10")?.blocks.isEmpty == true, "and fills no hours on Saturday")
        // A trip that began last week and one that runs on into the next.
        let edge = week(items([
            ghost(1, "Before", start: "2026-10-01", end: "2026-10-06", allDay: true),
            ghost(2, "After", start: "2026-10-10", end: "2026-10-15", allDay: true),
            ghost(3, "Gone", start: "2026-09-28", end: "2026-10-03", allDay: true),
            ghost(4, "Later", start: "2026-10-12", end: "2026-10-13", allDay: true),
        ]))
        expect(bar(edge, "Before").map { $0.firstColumn == 0 && !$0.startsHere && $0.endsHere } == true,
               "a trip from last week starts flat at Monday")
        expect(bar(edge, "After").map { $0.lastColumn == 6 && $0.startsHere && !$0.endsHere } == true,
               "one running on ends flat at Sunday")
        expect(bar(edge, "Gone") == nil && bar(edge, "Later") == nil,
               "trips wholly outside the week are not in it")
        expect(WeekGrid.stripItems(
            items([ghost(1, "Sunday", start: "2026-10-11", allDay: true)]), start: day("2026-10-05"), calendar: cal
        ).count == 1, "an all-day item on the week's last day is in the strip")
        let tail = week(items([ghost(1, "Tail", start: "2026-10-01", end: "2026-10-05", allDay: true)]))
        expect(bar(tail, "Tail").map { $0.firstColumn == 0 && $0.lastColumn == 0 && $0.endsHere } == true,
               "a trip whose LAST day is the week's first is still on that Monday")
        expect(WeekGrid.stripItems(
            items([ghost(1, "Next", start: "2026-10-12", allDay: true)]), start: day("2026-10-05"), calendar: cal
        ).isEmpty, "and one on the Monday after is not")
    }

    /// A trip takes the SAME lane in the week as in its month row, because it
    /// is the same lane logic over the same row.
    static func theStripIsTheMonthsLanes() {
        let i = items([
            ghost(1, "Lisbon", start: "2026-10-05", end: "2026-10-09", allDay: true),
            ghost(2, "Offsite", start: "2026-10-07", end: "2026-10-08", allDay: true),
            ghost(3, "Visit", start: "2026-10-10", end: "2026-10-11", allDay: true),
        ])
        let m = week(i)
        let month = MonthGrid.build(i, month: MonthGrid.monthStart(day("2026-10-08"), calendar: cal), calendar: cal)
        let row = month.weeks.first { $0.days.first?.day == day("2026-10-05") }
        for title in ["Lisbon", "Offsite", "Visit"] {
            let mine = bar(m, title)
            let theirs = row?.spans.first { $0.item.title == title }
            expect(mine != nil && mine?.lane == theirs?.lane, "\(title): the week's lane is the month's")
            expect(mine?.firstColumn == theirs?.firstColumn && mine?.lastColumn == theirs?.lastColumn,
                   "\(title): over the same columns")
        }
        expect(bar(m, "Lisbon")?.lane == 0 && bar(m, "Offsite")?.lane == 1, "the longer trip first, the other under it")
        expect(bar(m, "Visit")?.lane == 0, "a lane is free again the day after its trip ends")
        expect(m.lanes == 2 && WeekGrid.stripRows(m) == 2, "two lanes, two rows, no fold")
    }

    static func stripOverflow() {
        let i = items((1...6).map { ghost($0, "T\($0)", start: "2026-10-07", allDay: true) })
        let m = week(i)
        expect(m.bars.count == WeekGrid.stripMaxLanes && m.lanes == WeekGrid.stripMaxLanes,
               "the strip draws its lanes' worth")
        expect(m.overflow == [0, 0, 2, 0, 0, 0, 0], "and counts the rest on their day")
        expect(WeekGrid.stripRows(m) == WeekGrid.stripMaxLanes + 1, "the fold takes a row of its own")
        expect(column(m, "2026-10-07")?.order.count == 6, "the day's own list still walks all six")
        let roomy = week(i, maxLanes: 6)
        expect(roomy.overflow.allSatisfy { $0 == 0 } && WeekGrid.stripRows(roomy) == 6, "with room, nothing folds")
    }

    // MARK: edges

    static func overnightAndUnlikely() {
        let m = week(items([
            ghost(1, "Party", start: "2026-10-10T22:00:00-07:00", end: "2026-10-11T02:00:00-07:00"),
            ghost(2, "Maybe", start: "2026-10-08T18:00:00-07:00", commitment: .invited, band: .unlikely),
        ]))
        expect(block(m, "2026-10-10", "Party")?.segment.continuesAfter == true,
               "the party runs off Saturday's foot")
        expect(column(m, "2026-10-11")?.night.first?.segment.continuesBefore == true,
               "and its tail is in Sunday's night cap")
        expect(m.bars.isEmpty, "an overnight party is ink on the hours, never a bar")
        expect(column(m, "2026-10-08")?.unlikely.map(\.title) == ["Maybe"], "an unlikely plan is counted on its day")
        expect(column(m, "2026-10-08")?.blocks.isEmpty == true, "and not drawn")
    }

    /// The event pane's draft is one more row, so the week draws it wherever
    /// its fields put it: a block on its day, or a bar in the strip.
    static func theDraftIsOneOfTheBlocks() {
        var d = EventDraft()
        d.calendarId = "primary"
        d.date = "Oct 9"
        d.start = "2pm"
        d.end = "3pm"
        let today = at("2026-10-05T15:40:00-07:00")
        let timed = EventForm.provisional(d, color: nil, today: today, calendar: cal)
        let m = week(EventForm.withProvisional(items([]), timed, editing: nil))
        expect(column(m, "2026-10-09")?.blocks.map(\.segment.item.id) == [EventForm.provisionalId],
               "a timed draft is a block on its Friday")
        d.allDay = true
        d.endDate = "Oct 10"
        let allDay = EventForm.provisional(d, color: nil, today: today, calendar: cal)
        let s = week(EventForm.withProvisional(items([]), allDay, editing: nil))
        expect(s.bars.first.map { $0.item.id == EventForm.provisionalId && $0.firstColumn == 4 && $0.lastColumn == 5 } == true,
               "an all-day draft is a bar, Friday to Saturday")
        expect(s.columns.allSatisfy { $0.blocks.isEmpty }, "and no block")
    }

    // MARK: room and words

    static func widthsAndLabels() {
        expect(WeekGrid.columnWidth(width: 744, gutter: 44) == 100, "a seventh of what is left beside the gutter")
        expect(WeekGrid.columnWidth(width: 394, gutter: 44) == 50, "narrower when the container is: it follows down")
        expect(WeekGrid.columnWidth(width: 20, gutter: 44) == 0, "and never negative")
        let tue = day("2026-10-06")
        expect(WeekGrid.dayLabel(tue, columnWidth: 100, calendar: cal) == ("Tue", "6"), "Tue 6 with room")
        expect(WeekGrid.dayLabel(tue, columnWidth: WeekGrid.shortLabelWidth, calendar: cal).weekday == "Tue",
               "still whole at the threshold")
        expect(WeekGrid.dayLabel(tue, columnWidth: WeekGrid.shortLabelWidth - 1, calendar: cal) == ("T", "6"),
               "T 6 below it")
        expect(WeekGrid.showsBlockTime(columnWidth: WeekGrid.blockTimeMinWidth), "a block keeps its time at the threshold")
        expect(!WeekGrid.showsBlockTime(columnWidth: WeekGrid.blockTimeMinWidth - 1), "and is its title alone below it")
    }

    static func titles() {
        expect(WeekGrid.title(day("2026-10-05"), calendar: cal) == "Oct 5 to 11, 2026", "inside one month")
        expect(WeekGrid.title(day("2026-09-28"), calendar: cal) == "Sep 28 to Oct 4, 2026", "across a month")
        expect(WeekGrid.title(day("2026-12-28"), calendar: cal) == "Dec 28, 2026 to Jan 3, 2027", "across a year")
        for s in ["2026-10-05", "2026-09-28", "2026-12-28"] {
            expect(!WeekGrid.title(day(s), calendar: cal).contains("\u{2014}"), "\(s): no em dash")
        }
    }

    static func walking() {
        let order = items([
            ghost(1, "A", start: "2026-10-08T09:00:00-07:00", end: "2026-10-08T10:00:00-07:00"),
            ghost(2, "B", start: "2026-10-08T11:00:00-07:00", end: "2026-10-08T12:00:00-07:00"),
            ghost(3, "C", start: "2026-10-08T13:00:00-07:00", end: "2026-10-08T14:00:00-07:00"),
        ])
        let ids = order.map(\.id)
        expect(WeekGrid.step(order, from: nil, by: 1) == ids[0], "down from nothing is the first")
        expect(WeekGrid.step(order, from: nil, by: -1) == ids[2], "up from nothing is the last")
        expect(WeekGrid.step(order, from: ids[0], by: 1) == ids[1], "down one")
        expect(WeekGrid.step(order, from: ids[1], by: -1) == ids[0], "up one")
        expect(WeekGrid.step(order, from: ids[2], by: 1) == ids[2], "it stops at the last")
        expect(WeekGrid.step(order, from: ids[0], by: -1) == ids[0], "and at the first")
        expect(WeekGrid.step(order, from: "another day's", by: 1) == ids[0],
               "a selection that is not this day's starts over")
        expect(WeekGrid.step([], from: nil, by: 1) == nil, "nothing to walk in an empty day")
    }

    static func thePicker() {
        expect(CalendarViewMode.pickerOrder == [.month, .week, .agenda], "Month | Week | Agenda")
        expect(CalendarViewMode.initial == .month, "the month is still the default")
        expect(CalendarViewMode.pickerOrder.map(\.label) == ["Month", "Week", "Agenda"], "as the control reads")
        expect(CalendarViewMode.week.isGrid && CalendarViewMode.month.isGrid && !CalendarViewMode.agenda.isGrid,
               "the week shares the month's place; the agenda has its own")
        expect(CalendarViewMode.week.rawValue == "week", "the analytics word")
    }
}
