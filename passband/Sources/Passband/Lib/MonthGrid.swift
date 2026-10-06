// THE MONTH VIEW'S ARITHMETIC: the shape of the month. Which days the grid
// shows, what each day's cell lists and in what order, which items are spans
// across the top of the cells and which lane each takes, how much of a cell
// fits and what folds into "+N more", how many unlikely ghosts a day counts,
// how the keys move the selection, the day column beside the grid, and the
// range the page asks the daemon for.
//
// PURE, and Foundation only, like Lib/Agenda.swift beside it, so test.sh can
// hold it against fixtures (Tests/MonthGridTests.swift). Clock and zone are
// arguments; nothing here reads `Date()` or `Calendar.current`.
//
// A CONVENTIONAL MONTH (wave 3, the founder: "lets do top down like gmail
// does"). Each cell lists its day top-down: trips and stays first, as named
// bars across the cells they cover, in lanes every cell of the row keeps
// free; then the day's own all-day items; then its timed items by start, one
// row each. Whatever does not fit folds into "+N more", which opens the day.
// The hour strips of wave 2 are gone from the grid; their arithmetic lives on
// for the day column (`segments`, `nowMark`), whose hours are still a picture
// of a wall clock.

import Foundation

// MARK: - the page's mode

/// How the calendar page is drawn. The agenda is the daily surface; the month
/// is the shape of the month. There is no week or day view: the day is a
/// column beside the month.
enum CalendarViewMode: String, Sendable, Hashable {
    case agenda
    case month

    /// The page opens on the month (wave 3). The agenda is a key away.
    static let initial: CalendarViewMode = .month
    /// The header's switch, left to right: Month | Agenda.
    static let pickerOrder: [CalendarViewMode] = [.month, .agenda]
}

// MARK: - where the month is

/// The month on screen and the day selected in it.
struct MonthPlace: Hashable, Sendable {
    /// Local midnight on the 1st of the month shown.
    var month: Date
    /// Local midnight of the selected day: always one of the shown grid's days,
    /// which can be a greyed day of the month before or after.
    var day: Date
}

/// A key's move.
enum MonthMove: Hashable, Sendable {
    /// Arrows: left and right a day, up and down a week.
    case days(Int)
    /// `[` / `]`, `,` / `.`, and the header's arrows.
    case months(Int)
    /// `t`, and the header's Today.
    case today
}

// MARK: - the model

/// One timed item's piece of one day in the DAY COLUMN: either the folded
/// night row (midnight to 7am) or the hours (7am to midnight). `lo` / `hi` are fractions of
/// that part. A `continues` edge is drawn flat: the item goes on past it.
struct DaySegment: Hashable, Sendable {
    enum Part: String, Hashable, Sendable {
        case night
        case day
    }

    let item: AgendaItem
    let part: Part
    let lo: Double
    let hi: Double
    let continuesBefore: Bool
    let continuesAfter: Bool
}

/// One cell of the grid.
struct MonthDay: Hashable, Sendable, Identifiable {
    /// Local midnight.
    let day: Date
    /// False for the greyed days of the month before and after.
    let inMonth: Bool
    /// What the cell lists under its row's span lanes, top-down: this day's
    /// own all-day items, then the timed items that START this day, by start.
    /// A timed item that runs past midnight is listed once, where it begins
    /// (the day column shows its tail the morning after).
    let items: [AgendaItem]
    /// Unlikely ghosts that start this day: not drawn, counted.
    let unlikely: [AgendaItem]

    var id: Date { day }
}

/// What one cell shows in the room it has: the first `shown` of its items,
/// then "+`more` more" when anything is left over (items that did not fit,
/// and trips that found no lane over this day).
struct CellFold: Hashable, Sendable {
    let shown: Int
    let more: Int
}

/// One span's bar inside one week row.
struct SpanSegment: Hashable, Sendable, Identifiable {
    let item: AgendaItem
    /// Columns 0...6 of the row, inclusive.
    let firstColumn: Int
    let lastColumn: Int
    let lane: Int
    /// The span really begins (or ends) inside this row: a rounded end. A
    /// flat end runs on into the row before (or after).
    let startsHere: Bool
    let endsHere: Bool

    var id: String { "\(item.id)@\(firstColumn)" }
}

struct MonthWeek: Hashable, Sendable, Identifiable {
    let days: [MonthDay]
    let spans: [SpanSegment]
    /// Per column: spans covering that day that found no lane, so the cell
    /// can count them in its "+N more".
    let overflow: [Int]
    /// The span lanes this row uses: every cell in it lists its own items
    /// below them, so a trip's bar never lands on a dinner.
    let lanes: Int

    var id: Date { days.first?.day ?? .distantPast }
}

struct MonthModel: Hashable, Sendable {
    let month: Date
    let weeks: [MonthWeek]

    var days: [MonthDay] { weeks.flatMap(\.days) }
}

/// A segment placed in the day column: side by side with what it overlaps.
struct ColumnBlock: Hashable, Sendable, Identifiable {
    let segment: DaySegment
    let lane: Int
    let lanes: Int

    var id: String { "\(segment.item.id)|\(segment.part.rawValue)" }
}

/// The day column beside the month.
struct DayColumn: Hashable, Sendable {
    let day: Date
    /// The folded night row at the top.
    let night: [ColumnBlock]
    /// 7am to midnight.
    let blocks: [ColumnBlock]
    /// All-day items and spans covering the day: words above the hours, never
    /// blocks.
    let allDay: [AgendaItem]
    /// Unlikely ghosts starting this day: the "N unlikely" expander.
    let unlikely: [AgendaItem]
    /// What j / k step through, in the order drawn: the all-day words, then
    /// the timed blocks by start. Unlikely only while its expander is open,
    /// which is the page's to add.
    let order: [AgendaItem]
}

// MARK: - building

enum MonthGrid {
    /// Where the day column's hours start: the agenda's own 7am.
    static var dayFromHour: Int { Agenda.stripFromHour }
    static var nightHours: Double { Double(dayFromHour) }
    static var dayHours: Double { 24 - Double(dayFromHour) }

    /// How far before the grid the page asks: the timeline serves what STARTS
    /// in the range, so a trip that began before the first cell would
    /// otherwise be missing from the spans it still covers.
    static let spanLookbackDays = 31
    /// The daemon clamps a wider range (TIMELINE_MAX_SPAN in squelch-api).
    static let maxRequestDays = 400
    /// The daemon's default window after `from` (1 day behind + 90 ahead),
    /// which is what the agenda asks for by sending `from` alone.
    static let agendaWindowDays = 91
    /// Span lanes a row may use; more than this is counted, not drawn.
    static let maxLanes = 2

    // MARK: days

    /// Local midnight on the 1st of `date`'s month.
    static func monthStart(_ date: Date, calendar: Calendar) -> Date {
        let parts = calendar.dateComponents([.year, .month], from: date)
        guard let first = calendar.date(from: parts) else { return calendar.startOfDay(for: date) }
        return calendar.startOfDay(for: first)
    }

    /// `n` calendar days after a local midnight, as a local midnight. By
    /// calendar day, never by 86,400 seconds: a DST Sunday is 23 or 25 hours.
    static func addDays(_ n: Int, to day: Date, _ calendar: Calendar) -> Date {
        calendar.startOfDay(for: calendar.date(byAdding: .day, value: n, to: day) ?? day)
    }

    /// The first grid day and the midnight after the last: whole weeks from
    /// the user's first weekday on or before the 1st, through the week that
    /// holds the month's last day. Four to six rows.
    static func gridBounds(month: Date, calendar: Calendar) -> (start: Date, end: Date) {
        let first = monthStart(month, calendar: calendar)
        let lead = (calendar.component(.weekday, from: first) - calendar.firstWeekday + 7) % 7
        let length = calendar.range(of: .day, in: .month, for: first)?.count ?? 30
        let cells = (lead + length + 6) / 7 * 7
        let start = addDays(-lead, to: first, calendar)
        return (start, addDays(cells, to: start, calendar))
    }

    static func gridDays(month: Date, calendar: Calendar) -> [Date] {
        let (start, end) = gridBounds(month: month, calendar: calendar)
        var out: [Date] = []
        var day = start
        while day < end {
            out.append(day)
            day = addDays(1, to: day, calendar)
        }
        return out
    }

    /// The column headings, from the user's first weekday. English, as all the
    /// app's dates are.
    static func weekdayLabels(_ calendar: Calendar) -> [String] {
        let names = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"]
        return (0..<7).map { names[(calendar.firstWeekday - 1 + $0) % 7] }
    }

    // MARK: the day column's hours

    /// A wall clock's hours since `day`'s midnight, 0 before the day and 24
    /// after it.
    static func wallHours(_ t: Date, on day: Date, _ calendar: Calendar) -> Double {
        if t <= day { return 0 }
        if t >= addDays(1, to: day, calendar) { return 24 }
        let c = calendar.dateComponents([.hour, .minute, .second], from: t)
        return Double(c.hour ?? 0) + Double(c.minute ?? 0) / 60 + Double(c.second ?? 0) / 3600
    }

    /// Where a timed item sits on one day: up to two pieces, the night cap's
    /// and the waking strip's. Nothing for an all-day item, or one that misses
    /// the day. Something at exactly 7:00am is all strip; something over at
    /// exactly 7:00am is all night cap.
    static func segments(_ item: AgendaItem, on day: Date, calendar: Calendar) -> [DaySegment] {
        guard !item.allDay else { return [] }
        let next = addDays(1, to: day, calendar)
        let end = item.effectiveEnd
        guard item.start < next, end > day else { return [] }
        let h0 = nightHours
        let s = wallHours(item.start, on: day, calendar)
        // The hour the clocks go back reads the same wall time twice, so a
        // short item inside it can end "before" it began. A quarter hour, the
        // agenda strip's own minimum, keeps it on the strip.
        let e = min(24, max(wallHours(end, on: day, calendar), s + 0.25))
        var out: [DaySegment] = []
        if s < h0 {
            out.append(
                DaySegment(
                    item: item, part: .night, lo: s / h0, hi: min(e, h0) / h0,
                    continuesBefore: item.start < day, continuesAfter: e > h0))
        }
        if e > h0 {
            out.append(
                DaySegment(
                    item: item, part: .day,
                    lo: (max(s, h0) - h0) / dayHours, hi: (min(e, 24) - h0) / dayHours,
                    continuesBefore: s < h0, continuesAfter: end > next))
        }
        return out
    }

    /// Now, on the day column: which part and how far along it.
    static func nowMark(_ now: Date, day: Date, calendar: Calendar) -> (part: DaySegment.Part, at: Double)? {
        guard now >= day, now < addDays(1, to: day, calendar) else { return nil }
        let h = wallHours(now, on: day, calendar)
        return h < nightHours ? (.night, h / nightHours) : (.day, (h - nightHours) / dayHours)
    }

    // MARK: spans

    /// The last day an item covers. An all-day item's end is the exclusive
    /// midnight after its last day, and a timed one ending at midnight does
    /// not touch the day it ends on.
    static func lastDay(_ item: AgendaItem, calendar: Calendar) -> Date {
        let end = item.effectiveEnd
        return calendar.startOfDay(for: end > item.start ? end.addingTimeInterval(-1) : item.start)
    }

    /// A trip, a stay: all-day over more than one day, or timed and a day or
    /// longer. Ghost or real. Drawn as a named bar, never as ink on a strip,
    /// because a four-day stay would otherwise fill four strips solid and
    /// read as four full days of meetings. An overnight party is NOT a span:
    /// it is under a day, and its ink is the point.
    static func isSpan(_ item: AgendaItem, calendar: Calendar) -> Bool {
        if item.allDay {
            return lastDay(item, calendar: calendar) > calendar.startOfDay(for: item.start)
        }
        return item.effectiveEnd.timeIntervalSince(item.start) >= 86_400
    }

    /// The order spans claim lanes in: earliest first, the longer of two
    /// starting together first, then the agenda's own order.
    private static func spanPrecedes(_ a: AgendaItem, _ b: AgendaItem) -> Bool {
        if a.start != b.start { return a.start < b.start }
        if a.effectiveEnd != b.effectiveEnd { return a.effectiveEnd > b.effectiveEnd }
        return Agenda.precedes(a, b)
    }

    /// Each span's bar in each row it touches, in lanes: a span takes the
    /// lowest lane free across all its columns. Past `maxLanes` it is counted
    /// in `overflow` for every day it covers instead.
    static func spanRow(
        _ spans: [AgendaItem], rowStart: Date, calendar: Calendar, maxLanes: Int = maxLanes
    ) -> (segments: [SpanSegment], overflow: [Int]) {
        let rowLast = addDays(6, to: rowStart, calendar)
        let rowEnd = addDays(1, to: rowLast, calendar)
        func column(_ day: Date) -> Int {
            calendar.dateComponents([.day], from: rowStart, to: day).day ?? 0
        }
        var placed: [(item: AgendaItem, c0: Int, c1: Int, starts: Bool, ends: Bool)] = []
        for span in spans {
            let first = calendar.startOfDay(for: span.start)
            let last = lastDay(span, calendar: calendar)
            guard first < rowEnd, last >= rowStart else { continue }
            placed.append(
                (span, first <= rowStart ? 0 : column(first), last >= rowLast ? 6 : column(last),
                 first >= rowStart, last <= rowLast))
        }
        placed.sort { a, b in
            if a.c0 != b.c0 { return a.c0 < b.c0 }
            if a.c1 - a.c0 != b.c1 - b.c0 { return a.c1 - a.c0 > b.c1 - b.c0 }
            return spanPrecedes(a.item, b.item)
        }
        var laneEnds: [Int] = []
        var out: [SpanSegment] = []
        var overflow = Array(repeating: 0, count: 7)
        for p in placed {
            let lane = laneEnds.firstIndex { $0 < p.c0 } ?? laneEnds.count
            guard lane < maxLanes else {
                for c in p.c0...p.c1 { overflow[c] += 1 }
                continue
            }
            if lane == laneEnds.count { laneEnds.append(p.c1) } else { laneEnds[lane] = p.c1 }
            out.append(
                SpanSegment(
                    item: p.item, firstColumn: p.c0, lastColumn: p.c1, lane: lane,
                    startsHere: p.starts, endsHere: p.ends))
        }
        return (out, overflow)
    }

    /// The words on a span's bar: its title, and on the bar where it begins,
    /// a timed span's start ("6:05pm"). Nothing is said about where.
    static func spanLabel(_ segment: SpanSegment, calendar: Calendar) -> (title: String, sub: String?) {
        let sub = segment.startsHere && !segment.item.allDay
            ? Agenda.clock(segment.item.start, calendar) : nil
        return (segment.item.title, sub)
    }

    // MARK: the month

    /// The whole grid for one month, from the agenda's own items
    /// (`Agenda.items`): the same rows the agenda would draw, cut by day.
    static func build(
        _ items: [AgendaItem], month: Date, calendar: Calendar, maxLanes: Int = maxLanes
    ) -> MonthModel {
        let first = monthStart(month, calendar: calendar)
        let days = gridDays(month: first, calendar: calendar)
        guard let gridStart = days.first, let gridLast = days.last else {
            return MonthModel(month: first, weeks: [])
        }
        let gridEnd = addDays(1, to: gridLast, calendar)
        var index: [Date: Int] = [:]
        for (i, d) in days.enumerated() { index[d] = i }

        var listed = Array(repeating: [AgendaItem](), count: days.count)
        var unlikely = Array(repeating: [AgendaItem](), count: days.count)
        var spans: [AgendaItem] = []

        for item in items {
            let startDay = calendar.startOfDay(for: item.start)
            if item.folded {
                // Unlikely is counted on the day it starts, span or not, and
                // never drawn.
                if let i = index[startDay] { unlikely[i].append(item) }
                continue
            }
            if isSpan(item, calendar: calendar) {
                if startDay < gridEnd, lastDay(item, calendar: calendar) >= gridStart {
                    spans.append(item)
                }
                continue
            }
            if let i = index[startDay] { listed[i].append(item) }
        }

        spans.sort(by: spanPrecedes)
        var weeks: [MonthWeek] = []
        for row in stride(from: 0, to: days.count, by: 7) {
            let rowDays = (row..<min(row + 7, days.count)).map { i in
                MonthDay(
                    day: days[i],
                    inMonth: calendar.isDate(days[i], equalTo: first, toGranularity: .month),
                    items: listed[i].sorted(by: Agenda.precedes),
                    unlikely: unlikely[i].sorted(by: Agenda.precedes))
            }
            let (bars, overflow) = spanRow(
                spans, rowStart: days[row], calendar: calendar, maxLanes: maxLanes)
            let lanes = (bars.map(\.lane).max() ?? -1) + 1
            weeks.append(MonthWeek(days: rowDays, spans: bars, overflow: overflow, lanes: lanes))
        }
        return MonthModel(month: first, weeks: weeks)
    }

    // MARK: a cell's room

    /// How many rows fit under a cell's span lanes: the cell's height less
    /// its date band, its foot and the lanes, in whole rows. Every number is
    /// in points, as drawn (the view's metrics, scaled with density).
    static func slots(
        cellHeight: Double, lanes: Int, numberBand: Double, rowHeight: Double, rowGap: Double,
        foot: Double
    ) -> Int {
        let step = rowHeight + rowGap
        guard step > 0 else { return 0 }
        let room = cellHeight - numberBand - foot - Double(lanes) * step
        return max(0, Int(((room + rowGap) / step).rounded(.down)))
    }

    /// What a cell shows. Everything, when everything fits; otherwise all but
    /// one of its rows and "+N more" in the last, N counting the items left
    /// and the trips that found no lane. A cell with no room shows nothing
    /// and still counts.
    static func fold(items: Int, hiddenSpans: Int, slots: Int) -> CellFold {
        let total = items + hiddenSpans
        guard total > 0 else { return CellFold(shown: 0, more: 0) }
        if hiddenSpans == 0, items <= slots { return CellFold(shown: items, more: 0) }
        let shown = max(0, min(items, slots - 1))
        return CellFold(shown: shown, more: total - shown)
    }

    /// A cell's width: a seventh of the grid's, whatever that is now. The
    /// grid's width is the CONTAINER's, measured each layout, so narrowing
    /// the window narrows the cells (it once only ever grew).
    static func cellWidth(gridWidth: Double) -> Double { max(0, gridWidth / 7) }

    /// The narrowest cell that keeps a time beside a title ("7:45pm  Dreamers
    /// Dinner"). Below it a timed row is its title alone, and the time is in
    /// the row's help and the day column: a title cut to "Dre" beside a full
    /// time is a row that says when but not what.
    static let rowTimeMinWidth = 96.0

    /// The time a cell row leads with, or nil: all-day rows have none, and a
    /// narrow cell drops it.
    static func cellTime(
        _ item: AgendaItem, cellWidth: Double, minWidth: Double = rowTimeMinWidth, calendar: Calendar
    ) -> String? {
        guard !item.allDay, cellWidth >= minWidth else { return nil }
        return Agenda.clock(item.start, calendar)
    }

    /// Where now falls in today's list: the index of the first timed row
    /// that has not started yet (the count when all have). Nil with no timed
    /// rows, since a line among all-day bars would mean nothing.
    static func nowSlot(_ items: [AgendaItem], now: Date) -> Int? {
        guard items.contains(where: { !$0.allDay }) else { return nil }
        return items.firstIndex { !$0.allDay && $0.start > now } ?? items.count
    }

    // MARK: the day column

    /// The column for one day, from the same items.
    static func column(_ items: [AgendaItem], day: Date, calendar: Calendar) -> DayColumn {
        let day = calendar.startOfDay(for: day)
        let next = addDays(1, to: day, calendar)
        var allDay: [AgendaItem] = []
        var unlikely: [AgendaItem] = []
        var timed: [AgendaItem] = []
        var pieces: [DaySegment] = []
        for item in items {
            if item.folded {
                if calendar.startOfDay(for: item.start) == day { unlikely.append(item) }
                continue
            }
            guard item.start < next, item.effectiveEnd > day else { continue }
            if item.allDay || isSpan(item, calendar: calendar) {
                allDay.append(item)
                continue
            }
            let mine = segments(item, on: day, calendar: calendar)
            if !mine.isEmpty {
                timed.append(item)
                pieces.append(contentsOf: mine)
            }
        }
        allDay.sort(by: Agenda.precedes)
        timed.sort(by: Agenda.precedes)
        return DayColumn(
            day: day,
            night: lanes(pieces.filter { $0.part == .night }),
            blocks: lanes(pieces.filter { $0.part == .day }),
            allDay: allDay,
            unlikely: unlikely.sorted(by: Agenda.precedes),
            order: allDay + timed)
    }

    /// Side by side where they overlap: each run of overlapping blocks shares
    /// its width between as many lanes as it needs at once, and each block
    /// takes the first lane free when it starts. Touching is not overlapping:
    /// a 10am that follows a 9 to 10 sits under it, full width.
    static func lanes(_ pieces: [DaySegment]) -> [ColumnBlock] {
        let sorted = pieces.sorted { a, b in
            if a.lo != b.lo { return a.lo < b.lo }
            if a.hi != b.hi { return a.hi > b.hi }
            return Agenda.precedes(a.item, b.item)
        }
        var out: [ColumnBlock] = []
        var run: [(DaySegment, Int)] = []
        var laneEnds: [Double] = []
        var runEnd = -Double.infinity
        func close() {
            out.append(contentsOf: run.map { ColumnBlock(segment: $0.0, lane: $0.1, lanes: laneEnds.count) })
            run = []
            laneEnds = []
        }
        for piece in sorted {
            if !run.isEmpty, piece.lo >= runEnd { close() }
            if run.isEmpty { runEnd = piece.hi }
            if let lane = laneEnds.firstIndex(where: { $0 <= piece.lo }) {
                laneEnds[lane] = piece.hi
                run.append((piece, lane))
            } else {
                laneEnds.append(piece.hi)
                run.append((piece, laneEnds.count - 1))
            }
            runEnd = max(runEnd, piece.hi)
        }
        close()
        return out
    }

    /// A column block's time: "10 to 11am", "11am to 12:30pm", "8pm to ~11pm",
    /// "10pm to 6am Sun", or "until 6am" for one that began the day before.
    static func rangeText(_ item: AgendaItem, on day: Date, calendar: Calendar) -> String {
        let tilde = item.endEstimated ? "~" : ""
        guard let end = item.end, end > item.start else { return Agenda.clock(item.start, calendar) }
        if item.start < day { return "until \(tilde)\(Agenda.clock(end, calendar))" }
        let next = addDays(1, to: day, calendar)
        let endText = Agenda.clock(end, calendar)
            + (end > next ? " \(Agenda.format(end, "EEE", calendar))" : "")
        var startText = Agenda.clock(item.start, calendar)
        // "10 to 11am": the first meridiem goes when both share it.
        let h0 = calendar.component(.hour, from: item.start)
        let h1 = calendar.component(.hour, from: end)
        if end <= next, (h0 < 12) == (h1 < 12), !item.endEstimated {
            startText = String(startText.dropLast(2))
        }
        return "\(startText) to \(tilde)\(endText)"
    }

    // MARK: moving

    /// Today's place: today's month, today selected.
    static func place(today: Date, calendar: Calendar) -> MonthPlace {
        let day = calendar.startOfDay(for: today)
        return MonthPlace(month: monthStart(day, calendar: calendar), day: day)
    }

    /// Whether a day is one of the shown grid's cells.
    static func inGrid(_ day: Date, month: Date, calendar: Calendar) -> Bool {
        let (start, end) = gridBounds(month: month, calendar: calendar)
        return day >= start && day < end
    }

    /// A day picked by the pointer. A greyed day of the next month is still
    /// in this grid, so the month stays where it is.
    static func select(_ day: Date, in place: MonthPlace, calendar: Calendar) -> MonthPlace {
        let day = calendar.startOfDay(for: day)
        guard inGrid(day, month: place.month, calendar: calendar) else {
            return MonthPlace(month: monthStart(day, calendar: calendar), day: day)
        }
        return MonthPlace(month: place.month, day: day)
    }

    /// A key's move. Arrows stay in the shown month while the day they land
    /// on is still one of its cells, and turn the page only off the grid. A
    /// month step keeps the day of the month where it can (the 31st becomes
    /// the 30th, or the 28th); from a greyed day it lands on the 1st.
    static func navigate(
        _ place: MonthPlace, _ move: MonthMove, today: Date, calendar: Calendar
    ) -> MonthPlace {
        switch move {
        case .today:
            return Self.place(today: today, calendar: calendar)
        case .days(let n):
            return select(addDays(n, to: place.day, calendar), in: place, calendar: calendar)
        case .months(let n):
            let target = monthStart(
                calendar.date(byAdding: .month, value: n, to: place.month) ?? place.month,
                calendar: calendar)
            let inMonth = calendar.isDate(place.day, equalTo: place.month, toGranularity: .month)
            let wanted = inMonth ? calendar.component(.day, from: place.day) : 1
            let length = calendar.range(of: .day, in: .month, for: target)?.count ?? 28
            return MonthPlace(month: target, day: addDays(min(wanted, length) - 1, to: target, calendar))
        }
    }

    // MARK: the request

    /// What the page asks the daemon for while a month is on screen: the
    /// agenda's own window (so going back to it never shows a hole) together
    /// with the grid and a lookback for spans that began before it. When the
    /// two together are wider than the daemon serves (a month far from
    /// today), the month alone.
    static func requestRange(month: Date, now: Date, calendar: Calendar) -> (from: Date, to: Date) {
        let (gridStart, gridEnd) = gridBounds(month: month, calendar: calendar)
        let monthFrom = addDays(-spanLookbackDays, to: gridStart, calendar)
        let agendaFrom = addDays(-1, to: calendar.startOfDay(for: now), calendar)
        let agendaTo = addDays(agendaWindowDays, to: agendaFrom, calendar)
        let from = min(monthFrom, agendaFrom)
        let to = max(gridEnd, agendaTo)
        // Elapsed days, as the daemon counts its clamp: a calendar count can
        // be an hour short of it across a DST change.
        if to.timeIntervalSince(from) <= Double(maxRequestDays) * 86_400 { return (from, to) }
        return (monthFrom, gridEnd)
    }

    // MARK: words

    /// "October 2026".
    static func title(_ month: Date, calendar: Calendar) -> String {
        Agenda.format(month, "MMMM yyyy", calendar)
    }

    /// A cell's date: the number, or "Oct 1" on a 1st and "Sep 28" on a
    /// greyed day, so the month a day belongs to is never a guess.
    static func cellLabel(_ day: Date, month: Date, calendar: Calendar) -> String {
        let inMonth = calendar.isDate(day, equalTo: month, toGranularity: .month)
        if !inMonth || calendar.component(.day, from: day) == 1 {
            return Agenda.format(day, "MMM d", calendar)
        }
        return Agenda.format(day, "d", calendar)
    }
}
