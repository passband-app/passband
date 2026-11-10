// THE WEEK VIEW'S ARITHMETIC: which seven days, each day's hours, and the
// strip of all-day items and trips across the top.
//
// PURE, and Foundation only, like Lib/MonthGrid.swift beside it
// (Tests/WeekGridTests.swift). Clock and zone are arguments.
//
// NOTHING NEW IS INVENTED HERE (wave 6, the founder: "lets add a week view
// too"). A week is seven of the month's day columns over one hours axis, so
// each day is `MonthGrid.column` as it stands: the night folded into its cap,
// the hours from 7am, overlapping blocks side by side (`MonthGrid.lanes`).
// The strip across the top is one of the month's week rows: the same bars in
// the same lanes (`MonthGrid.spanRow`), so a trip reads the same in both
// views. The only difference is who is in the strip: in the month a single
// all-day item is a row in its own cell, and here, where the cells are hours,
// it is a one-day bar.

import Foundation

/// One week, as drawn.
struct WeekModel: Hashable, Sendable {
    /// Local midnight of the first day (the user's first weekday).
    let start: Date
    /// Seven days, each the month's own day column.
    let columns: [DayColumn]
    /// The strip: all-day items and trips, in the month's lanes.
    let bars: [SpanSegment]
    /// Per column: strip items that found no lane ("+N more").
    let overflow: [Int]
    /// The lanes the strip uses.
    let lanes: Int

    var days: [Date] { columns.map(\.day) }
}

enum WeekGrid {
    /// The lanes the strip draws before a day says "+N more". Enough for a
    /// trip, a stay and a birthday at once; past it the hours would be pushed
    /// off a short window.
    static let stripMaxLanes = 4

    // MARK: days

    /// The first day of the week `day` is in: the user's first weekday on or
    /// before it, as a local midnight. The month grid's rows start on the
    /// same day (`MonthGrid.gridBounds`).
    static func weekStart(_ day: Date, calendar: Calendar) -> Date {
        let day = calendar.startOfDay(for: day)
        let lead = (calendar.component(.weekday, from: day) - calendar.firstWeekday + 7) % 7
        return MonthGrid.addDays(-lead, to: day, calendar)
    }

    /// The seven days from `start`, by calendar day (a DST week is 167 or
    /// 169 hours).
    static func days(from start: Date, calendar: Calendar) -> [Date] {
        (0..<7).map { MonthGrid.addDays($0, to: start, calendar) }
    }

    // MARK: the strip

    /// What the strip holds: everything the day columns say as words rather
    /// than blocks (all-day items, and timed ones a day or longer), that
    /// touches the week. Unlikely ghosts are counted on their day, not drawn.
    static func stripItems(_ items: [AgendaItem], start: Date, calendar: Calendar) -> [AgendaItem] {
        let end = MonthGrid.addDays(7, to: start, calendar)
        return items.filter { item in
            guard !item.folded, item.allDay || MonthGrid.isSpan(item, calendar: calendar) else { return false }
            return calendar.startOfDay(for: item.start) < end
                && MonthGrid.lastDay(item, calendar: calendar) >= start
        }
    }

    // MARK: the week

    /// The week `day` is in, from the agenda's own items.
    static func build(
        _ items: [AgendaItem], day: Date, calendar: Calendar, maxLanes: Int = stripMaxLanes
    ) -> WeekModel {
        let start = weekStart(day, calendar: calendar)
        let columns = days(from: start, calendar: calendar).map {
            MonthGrid.column(items, day: $0, calendar: calendar)
        }
        let (bars, overflow) = MonthGrid.spanRow(
            stripItems(items, start: start, calendar: calendar), rowStart: start, calendar: calendar,
            maxLanes: maxLanes)
        return WeekModel(
            start: start, columns: columns, bars: bars, overflow: overflow,
            lanes: (bars.map(\.lane).max() ?? -1) + 1)
    }

    /// The rows the strip takes: its lanes, and one more when any day has
    /// something that found none (the "+N more" line). Zero is no strip.
    static func stripRows(_ model: WeekModel) -> Int {
        model.lanes + (model.overflow.contains { $0 > 0 } ? 1 : 0)
    }

    // MARK: room

    /// A day column's width: a seventh of what is left beside the hours'
    /// gutter, whatever that is now (the month's rule: width comes from the
    /// container, both ways).
    static func columnWidth(width: Double, gutter: Double) -> Double {
        max(0, (width - gutter) / 7)
    }

    /// Below this a column head is a letter and the date ("T 6"); at or
    /// above it, "Tue 6".
    static let shortLabelWidth = 58.0
    /// Below this a block is its title alone; its time is in its help and
    /// the detail.
    static let blockTimeMinWidth = 96.0
    /// Below this a day's unlikely count is not printed (it is in the help).
    static let unlikelyMinWidth = 124.0

    /// A column's head: the weekday (short at narrow widths) and the date.
    static func dayLabel(_ day: Date, columnWidth: Double, calendar: Calendar) -> (weekday: String, number: String) {
        let name = Agenda.format(day, "EEE", calendar)
        return (columnWidth < shortLabelWidth ? String(name.prefix(1)) : name, Agenda.format(day, "d", calendar))
    }

    static func showsBlockTime(columnWidth: Double) -> Bool { columnWidth >= blockTimeMinWidth }

    // MARK: words

    /// "Oct 4 to 10, 2026"; across a month "Sep 27 to Oct 3, 2026"; across a
    /// year "Dec 27, 2026 to Jan 2, 2027".
    static func title(_ start: Date, calendar: Calendar) -> String {
        let last = MonthGrid.addDays(6, to: start, calendar)
        let year = Agenda.format(last, "yyyy", calendar)
        if !calendar.isDate(start, equalTo: last, toGranularity: .year) {
            return "\(Agenda.format(start, "MMM d, yyyy", calendar)) to \(Agenda.format(last, "MMM d", calendar)), \(year)"
        }
        let first = Agenda.format(start, "MMM d", calendar)
        if calendar.isDate(start, equalTo: last, toGranularity: .month) {
            return "\(first) to \(Agenda.format(last, "d", calendar)), \(year)"
        }
        return "\(first) to \(Agenda.format(last, "MMM d", calendar)), \(year)"
    }

    // MARK: walking

    /// j / k through one day's items. From nothing selected, down lands on
    /// the first and up on the last; at either end it stays. Nil with nothing
    /// to walk.
    static func step(_ order: [AgendaItem], from current: String?, by delta: Int) -> String? {
        guard !order.isEmpty else { return nil }
        guard let at = current.flatMap({ id in order.firstIndex { $0.id == id } }) else {
            return delta > 0 ? order.first?.id : order.last?.id
        }
        return order[max(0, min(order.count - 1, at + delta))].id
    }
}
