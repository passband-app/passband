// THE CALENDAR PAGE'S MONTH: the shape of the month, and the selected day's
// column beside it.
//
// The design is docs/calendar-ux-preview.html, section 4, and it is binding:
//   - No titles in the cells. Each day is a strip of its waking hours, 7am to
//     midnight, after a folded night cap. Load is the amount of ink: no heat
//     colour, no number.
//   - Midnight is the cell border, so a week row reads as one timeline and a
//     party that runs past midnight lands in the next cell's night cap.
//   - Spans carry the only words: trips and stays are named bars across the
//     top of the cells they cover.
//   - The fills are the agenda's: solid is real or committed, striped is
//     likely, an outline is maybe, unlikely is a count under the day. Blue is
//     state only: today, now, the selection.
//
// THE LEVELS. The month is a stack: the grid, the selected day's column, the
// selected item's detail (`CalendarDetail`, the agenda's own). A wide window
// shows the grid and one pane beside it, which is the column, or the detail
// when an item is open (with a way back to the column). A narrow one shows
// only the deepest open level, each with a way back. Escape closes them from
// the deepest (`AppStore.calendarMonthBack`).
//
// All of the arithmetic is Lib/MonthGrid.swift, pure and tested; this file
// only draws it.

#if os(macOS)
    import SwiftUI

    // MARK: - the header switch

    /// Agenda or month, in the page header. Two options rather than the mock's
    /// Day / Week / Month: the day is the column beside the month and there
    /// is no week, so a control offering either would be a lie, and a
    /// disabled one would be furniture.
    struct CalendarModeSwitch: View {
        @Environment(AppStore.self) private var store

        var body: some View {
            GlassSegmented(
                options: [(CalendarViewMode.agenda, "Agenda"), (CalendarViewMode.month, "Month")],
                selection: Binding(
                    get: { store.calendarMode },
                    set: { store.setCalendarMode($0) }))
        }
    }

    // MARK: - metrics

    /// The month's measurements, in one place so the cell, the span layer and
    /// the minimum row height agree. Computed, never stored: they follow the
    /// density setting (`Space.scaled`).
    @MainActor
    enum MonthMetrics {
        static var numberTop: CGFloat { Space.s6 }
        static var numberHeight: CGFloat { Space.scaled(17) }
        static var laneHeight: CGFloat { Space.scaled(17) }
        static var laneGap: CGFloat { 2 }
        static var stripHeight: CGFloat { Space.scaled(20) }
        /// The folded night cap at the start of every strip.
        static var capWidth: CGFloat { Space.scaled(10) }
        /// The strip's inset at a row's two ends, where it is rounded.
        static var rowInset: CGFloat { Space.s6 }
        static var noteHeight: CGFloat { Space.scaled(13) }

        static func lanesTop() -> CGFloat { numberTop + numberHeight + Space.s2 }

        /// Every row reserves room for at least one lane, so the strips sit
        /// on one line down the month whether or not anyone travels.
        static func stripTop(lanes: Int) -> CGFloat {
            lanesTop() + CGFloat(max(1, lanes)) * (laneHeight + laneGap) + Space.s4
        }

        static func barsTop(lanes: Int) -> CGFloat { stripTop(lanes: lanes) + stripHeight + 3 }
        static func noteTop(lanes: Int) -> CGFloat { stripTop(lanes: lanes) + stripHeight + Space.scaled(9) }
        static func minRow(lanes: Int) -> CGFloat { noteTop(lanes: lanes) + noteHeight + Space.s6 }
    }

    // MARK: - the month page

    /// The whole month mode under the page's status lines: the month bar, and
    /// the grid with its pane.
    struct CalendarMonthView: View {
        @Environment(AppStore.self) private var store
        let model: MonthModel
        let column: DayColumn
        let place: MonthPlace
        /// The item whose detail is open, if any.
        let item: AgendaItem?
        /// The grid and a pane side by side; otherwise one level at a time.
        let twoPanes: Bool
        let paneWidth: CGFloat
        let unlikelyOpen: Bool
        let toggleUnlikely: () -> Void
        let now: Date
        let calendar: Calendar

        var body: some View {
            VStack(spacing: 0) {
                MonthBar(title: MonthGrid.title(place.month, calendar: calendar))
                HStack(alignment: .top, spacing: Space.scaled(18)) {
                    if twoPanes || (item == nil && !store.calendarDayOpen) {
                        MonthGridView(
                            model: model, place: place, item: item, twoPanes: twoPanes,
                            now: now, calendar: calendar)
                    }
                    if twoPanes || item != nil || store.calendarDayOpen {
                        pane
                            .frame(width: twoPanes ? paneWidth : nil)
                            .frame(maxWidth: twoPanes ? paneWidth : .infinity, maxHeight: .infinity, alignment: .top)
                    }
                }
                .padding(.horizontal, Space.scaled(22))
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
            }
        }

        @ViewBuilder
        private var pane: some View {
            if let item {
                ScrollView(.vertical) {
                    VStack(alignment: .leading, spacing: Space.s10) {
                        BackLink(text: Agenda.format(place.day, "EEE MMM d", calendar)) {
                            store.calendarMonthItem = nil
                        }
                        CalendarDetail(item: item, calendar: calendar)
                            // Keyed by the item: a different item is a
                            // different detail, its sources loading anew.
                            .id(item.id)
                    }
                    .padding(.top, Space.s4)
                    .padding(.bottom, Space.scaled(28))
                }
                .scrollBounceBehavior(.basedOnSize)
                .scrollIndicators(.hidden)
            } else {
                DayColumnPane(
                    column: column,
                    back: twoPanes ? nil : MonthGrid.title(place.month, calendar: calendar),
                    unlikelyOpen: unlikelyOpen,
                    toggleUnlikely: toggleUnlikely,
                    now: now,
                    calendar: calendar)
            }
        }
    }

    /// "‹ Thu Oct 8": one level back up the stack.
    private struct BackLink: View {
        let text: String
        let action: () -> Void

        var body: some View {
            Button(action: action) {
                HStack(spacing: Space.s4) {
                    Image(systemName: "chevron.left")
                        .font(Typo.ui(10, weight: .semibold))
                    Text(text)
                        .font(Typo.rowSub)
                }
                .padding(.vertical, Space.s2)
            }
            .buttonStyle(.textAction)
            .pointingHand()
            .help("Back (Esc)")
        }
    }

    /// The month's own bar: which month, the way to the ones beside it, and
    /// back to today. The page header above it carries the mode switch.
    private struct MonthBar: View {
        @Environment(AppStore.self) private var store
        let title: String

        var body: some View {
            HStack(spacing: Space.s8) {
                ChromeChip(icon: "chevron.left", help: "Previous month ( [ )") {
                    store.moveCalendarMonth(.months(-1))
                }
                ChromeChip(icon: "chevron.right", help: "Next month ( ] )") {
                    store.moveCalendarMonth(.months(1))
                }
                Text(title)
                    .font(Typo.ui(15, weight: .semibold))
                    .foregroundStyle(Palette.ink)
                    .monospacedDigit()
                    .padding(.leading, Space.s4)
                ChromeChip(text: "Today", help: "Today ( t )") {
                    store.moveCalendarMonth(.today)
                }
                Spacer(minLength: 0)
            }
            .padding(.horizontal, Space.scaled(22))
            .padding(.top, Space.s12)
            .padding(.bottom, Space.s10)
        }
    }

    // MARK: - the grid

    private struct MonthGridView: View {
        @Environment(AppStore.self) private var store
        let model: MonthModel
        let place: MonthPlace
        let item: AgendaItem?
        let twoPanes: Bool
        let now: Date
        let calendar: Calendar

        @State private var size: CGSize = .zero

        var body: some View {
            let rows = max(1, model.weeks.count)
            let headings = MonthGrid.weekdayLabels(calendar)
            let cellWidth = size.width / 7
            // The rows share the height there is, and never go below what a
            // cell needs; past that the grid scrolls.
            let rowHeight = max(
                MonthMetrics.minRow(lanes: model.lanes),
                floor(size.height / CGFloat(rows)))
            VStack(alignment: .leading, spacing: 0) {
                HStack(spacing: 0) {
                    ForEach(headings.indices, id: \.self) { i in
                        Text(headings[i])
                            .font(Typo.ui(10, weight: .medium))
                            .foregroundStyle(Palette.inkFaintest)
                            .padding(.leading, Space.s10)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }
                }
                .padding(.bottom, Space.s6)
                ScrollView(.vertical) {
                    VStack(spacing: 0) {
                        ForEach(model.weeks) { week in
                            MonthWeekRow(
                                week: week, lanes: model.lanes, month: model.month, place: place,
                                item: item, twoPanes: twoPanes, cellWidth: cellWidth,
                                height: rowHeight, now: now, calendar: calendar)
                        }
                    }
                }
                .scrollBounceBehavior(.basedOnSize)
                .scrollIndicators(.hidden)
                .onGeometryChange(for: CGSize.self) { $0.size } action: { size = $0 }
                MonthFooter(loading: !store.calendarLoaded)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
            .accessibilityElement(children: .contain)
            .accessibilityLabel(MonthGrid.title(model.month, calendar: calendar))
        }
    }

    /// The one line that says how to read an unconventional grid, and its keys.
    private struct MonthFooter: View {
        let loading: Bool

        var body: some View {
            HStack(spacing: Space.s12) {
                Text(loading ? "loading your calendar…" : "Each day: a folded night cap, then 7am to midnight.")
                    .font(Typo.micro)
                    .foregroundStyle(Palette.inkFaintest)
                    .lineLimit(1)
                Spacer(minLength: Space.s8)
                KeyHint(["←", "→", "↑", "↓"], "move")
                KeyHint("↵", "open")
            }
            .padding(.vertical, Space.s10)
        }
    }

    // MARK: - a week row

    private struct MonthWeekRow: View {
        @Environment(AppStore.self) private var store
        let week: MonthWeek
        let lanes: Int
        let month: Date
        let place: MonthPlace
        let item: AgendaItem?
        let twoPanes: Bool
        let cellWidth: CGFloat
        let height: CGFloat
        let now: Date
        let calendar: Calendar

        var body: some View {
            ZStack(alignment: .topLeading) {
                HStack(spacing: 0) {
                    ForEach(Array(week.days.enumerated()), id: \.element.id) { i, day in
                        MonthCell(
                            day: day,
                            column: i,
                            label: MonthGrid.cellLabel(day.day, month: month, calendar: calendar),
                            selected: day.day == place.day,
                            today: calendar.isDate(day.day, inSameDayAs: now),
                            overflow: i < week.overflow.count ? week.overflow[i] : 0,
                            lanes: lanes,
                            width: cellWidth,
                            now: now,
                            calendar: calendar
                        ) {
                            store.selectCalendarDay(day.day, open: !twoPanes)
                        }
                    }
                }
                ForEach(week.spans) { span in
                    SpanBar(
                        segment: span,
                        label: MonthGrid.spanLabel(span, calendar: calendar),
                        selected: item?.id == span.item.id,
                        cellWidth: cellWidth
                    ) {
                        store.calendarMonthItem = span.item.id
                    }
                    .offset(
                        x: CGFloat(span.firstColumn) * cellWidth + (span.startsHere ? MonthMetrics.rowInset : 0),
                        y: MonthMetrics.lanesTop() + CGFloat(span.lane) * (MonthMetrics.laneHeight + MonthMetrics.laneGap))
                }
            }
            .frame(height: height, alignment: .top)
            .overlay(alignment: .top) { Hairline() }
        }
    }

    // MARK: - a cell

    private struct MonthCell: View {
        let day: MonthDay
        let column: Int
        let label: String
        let selected: Bool
        let today: Bool
        let overflow: Int
        let lanes: Int
        let width: CGFloat
        let now: Date
        let calendar: Calendar
        let action: () -> Void

        var body: some View {
            let shape = RoundedRectangle(cornerRadius: 9, style: .continuous)
            ZStack(alignment: .topLeading) {
                numberRow
                    .padding(.horizontal, Space.s10)
                    .frame(width: width, height: MonthMetrics.numberHeight, alignment: .leading)
                    .offset(y: MonthMetrics.numberTop)
                DayStrip(
                    day: day,
                    weekStart: column == 0,
                    weekEnd: column == 6,
                    width: width,
                    now: today ? now : nil,
                    calendar: calendar
                )
                .opacity(day.inMonth ? 1 : 0.45)
                .offset(y: MonthMetrics.stripTop(lanes: lanes))
                allDayBars
                    .offset(y: MonthMetrics.barsTop(lanes: lanes))
                if !day.unlikely.isEmpty {
                    Text("\(day.unlikely.count) unlikely")
                        .font(Typo.micro)
                        .foregroundStyle(Palette.inkFaintest)
                        .lineLimit(1)
                        .padding(.leading, Space.s10)
                        .frame(width: width, alignment: .leading)
                        .offset(y: MonthMetrics.noteTop(lanes: lanes))
                }
            }
            .frame(width: width, alignment: .topLeading)
            .frame(maxHeight: .infinity, alignment: .top)
            // The selection is a wash and an EVEN 1pt edge, all the way round;
            // the edge sits over the strip so nothing breaks it.
            .background(shape.fill(Palette.accent.opacity(selected ? 0.13 : 0)).padding(1))
            .overlay(shape.strokeBorder(Palette.accent.opacity(selected ? 0.45 : 0), lineWidth: 1).padding(1))
            .contentShape(Rectangle())
            .onTapGesture(perform: action)
            .accessibilityElement(children: .ignore)
            .accessibilityLabel(accessibility)
            .accessibilityAddTraits(selected ? [.isButton, .isSelected] : .isButton)
            .accessibilityAction(.default, action)
        }

        private var numberRow: some View {
            HStack(alignment: .firstTextBaseline, spacing: Space.s4) {
                Text(label)
                    .font(Typo.num(12, weight: today ? .bold : .medium))
                    .foregroundStyle(today ? Palette.accentInk : (day.inMonth ? Palette.inkDim : Palette.inkFaintest))
                    .lineLimit(1)
                Spacer(minLength: 0)
                if overflow > 0 {
                    Text("+\(overflow)")
                        .font(Typo.micro)
                        .foregroundStyle(Palette.inkFaintest)
                        .help("\(overflow) more \(overflow == 1 ? "trip or stay" : "trips and stays") than there is room for")
                }
                if today {
                    Text("today")
                        .font(Typo.micro)
                        .foregroundStyle(Palette.accentInk)
                }
            }
        }

        /// All-day items of this one day: a faint bar each under the strip,
        /// never a block, because a bill due is not an hour taken. Two at
        /// most; the column lists them all.
        private var allDayBars: some View {
            let inset = MonthMetrics.rowInset
            let bars = Array(day.allDay.prefix(2))
            return VStack(alignment: .leading, spacing: 1.5) {
                ForEach(bars) { item in
                    Capsule()
                        .fill(CalendarInk.account.opacity(item.cancelled ? 0.14 : 0.32))
                        .frame(height: 2.5)
                }
            }
            .padding(.leading, column == 0 ? inset : 0)
            .padding(.trailing, column == 6 ? inset : 1)
            .frame(width: width)
            .opacity(day.inMonth ? 1 : 0.45)
        }

        private var accessibility: String {
            var parts = [Agenda.format(day.day, "EEEE MMMM d", calendar)]
            let timed = Set(day.segments.map(\.item.id)).count
            if timed > 0 { parts.append("\(timed) timed") }
            if !day.allDay.isEmpty { parts.append("\(day.allDay.count) all day") }
            if !day.unlikely.isEmpty { parts.append("\(day.unlikely.count) unlikely") }
            return parts.joined(separator: ", ")
        }
    }

    // MARK: - the strip

    /// One day's hours, left to right: the folded night cap (midnight to
    /// 7am), a hairline gap, then 7am to midnight, ending a point short of the
    /// cell's edge, which is midnight. Rounded only at a row's two ends, so a
    /// week reads as one strip.
    private struct DayStrip: View {
        let day: MonthDay
        let weekStart: Bool
        let weekEnd: Bool
        let width: CGFloat
        let now: Date?
        let calendar: Calendar

        var body: some View {
            let h = MonthMetrics.stripHeight
            let lead = weekStart ? MonthMetrics.rowInset : 0
            let trail = weekEnd ? MonthMetrics.rowInset : 0
            let cap = MonthMetrics.capWidth
            let dayX = lead + cap + 1
            let dayW = max(0, width - dayX - trail - 1)
            let r: CGFloat = 4
            ZStack(alignment: .topLeading) {
                UnevenRoundedRectangle(
                    topLeadingRadius: weekStart ? r : 0, bottomLeadingRadius: weekStart ? r : 0,
                    bottomTrailingRadius: weekEnd ? r : 0, topTrailingRadius: weekEnd ? r : 0,
                    style: .continuous)
                    .fill(Palette.hairline.opacity(0.45))
                    .frame(width: max(0, width - lead - trail), height: h)
                    .offset(x: lead)
                // The fold: the same strip, a shade apart.
                UnevenRoundedRectangle(
                    topLeadingRadius: weekStart ? r : 0, bottomLeadingRadius: weekStart ? r : 0,
                    bottomTrailingRadius: 0, topTrailingRadius: 0, style: .continuous)
                    .fill(Palette.hairline.opacity(0.4))
                    .frame(width: cap, height: h)
                    .offset(x: lead)
                ForEach(Array(day.segments.enumerated()), id: \.offset) { _, s in
                    let x0 = s.part == .night ? lead : dayX
                    let w0 = s.part == .night ? cap : dayW
                    AgendaMark(
                        fill: s.item.fill, estimated: s.item.endEstimated,
                        thin: s.item.cancelled, cornerRadius: 2.5,
                        flatLeading: s.continuesBefore, flatTrailing: s.continuesAfter,
                        tailOnly: s.continuesBefore, feather: true)
                        // A quarter hour is a sliver at this scale; two
                        // points is the least that still reads as a mark.
                        .frame(width: max(2, w0 * (s.hi - s.lo) - (s.continuesAfter ? 0 : 1)), height: h)
                        .offset(x: x0 + w0 * s.lo)
                }
                if let now, let mark = MonthGrid.nowMark(now, day: day.day, calendar: calendar) {
                    let x = mark.part == .night ? lead + cap * mark.at : dayX + dayW * mark.at
                    Capsule()
                        .fill(Palette.accent)
                        .frame(width: 2, height: h + 6)
                        .offset(x: x - 1, y: -3)
                }
            }
            .frame(width: width, height: h, alignment: .topLeading)
            .allowsHitTesting(false)
        }
    }

    // MARK: - a span

    /// A trip or a stay across the top of the cells it covers, in its fill.
    /// Its words are at full strength whatever the fill: a ghost's container
    /// thins, never its ink.
    private struct SpanBar: View {
        let segment: SpanSegment
        let label: (title: String, sub: String?)
        let selected: Bool
        let cellWidth: CGFloat
        let action: () -> Void

        var body: some View {
            let item = segment.item
            let inset = MonthMetrics.rowInset
            let cells = CGFloat(segment.lastColumn - segment.firstColumn + 1)
            let width = max(
                0,
                cells * cellWidth - (segment.startsHere ? inset : 0) - (segment.endsHere ? inset : 0))
            let r: CGFloat = 6
            let lead: CGFloat = segment.startsHere ? r : 0
            let trail: CGFloat = segment.endsHere ? r : 0
            let shape = UnevenRoundedRectangle(
                topLeadingRadius: lead, bottomLeadingRadius: lead,
                bottomTrailingRadius: trail, topTrailingRadius: trail, style: .continuous)
            Button(action: action) {
                HStack(alignment: .firstTextBaseline, spacing: Space.s6) {
                    Text(label.title)
                        .font(Typo.ui(10.5, weight: .semibold))
                        .foregroundStyle(Palette.ink)
                        .strikethrough(item.cancelled, color: Palette.inkFaint)
                    if let sub = label.sub {
                        Text(sub)
                            .font(Typo.ui(10.5))
                            .foregroundStyle(Palette.inkDim)
                    }
                }
                .lineLimit(1)
                .padding(.horizontal, Space.s8)
                .frame(width: width, height: MonthMetrics.laneHeight, alignment: .leading)
                .background { BlockFill(fill: item.fill, thin: item.cancelled, shape: shape) }
                .overlay(shape.strokeBorder(Palette.accent.opacity(selected ? 0.9 : 0), lineWidth: 1.5))
                .clipShape(shape)
                .contentShape(shape)
            }
            .buttonStyle(.plain)
            .pointingHand()
            .help(item.title)
        }
    }

    /// The column's and the span's fill: the agenda's scale at the size where
    /// words sit on it. Solid is the account's hue, quiet enough to read ink
    /// on; striped is hatched and edged; an outline is an even edge alone.
    struct BlockFill<S: InsettableShape>: View {
        let fill: AgendaFill
        var thin = false
        let shape: S
        var hue: Color = CalendarInk.account

        var body: some View {
            ZStack {
                switch thin ? .outline : fill {
                case .solid:
                    shape.fill(hue.opacity(0.30))
                case .striped:
                    shape.fill(hue.opacity(0.09))
                    Stripes(spacing: 7)
                        .stroke(hue.opacity(0.30), lineWidth: 2)
                        .clipShape(shape)
                    shape.strokeBorder(hue.opacity(0.62), lineWidth: 1)
                case .outline:
                    shape.strokeBorder(hue.opacity(thin ? 0.35 : 0.5), lineWidth: 1)
                }
            }
        }
    }

    // MARK: - the day column

    /// The selected day, hours down the side: the night folded into a thin
    /// row at the top, real events solid, ghosts by band, guessed ends soft,
    /// all-day items as words above, and the unlikely folded into a count.
    private struct DayColumnPane: View {
        @Environment(AppStore.self) private var store
        let column: DayColumn
        /// The month's name, for the way back on a narrow window; nil when
        /// the grid is beside it.
        let back: String?
        let unlikelyOpen: Bool
        let toggleUnlikely: () -> Void
        let now: Date
        let calendar: Calendar

        @State private var visibleHeight: CGFloat = 0
        @State private var headHeight: CGFloat = 0

        private var nightHeight: CGFloat { Space.scaled(22) }

        var body: some View {
            // The hours fill what is left under the header, between a floor
            // where a half hour still holds a line of text and a ceiling
            // past which a short day looks stretched.
            let room = visibleHeight - headHeight - nightHeight - Space.scaled(28)
            let pph = min(Space.scaled(44), max(Space.scaled(20), floor(room / CGFloat(MonthGrid.dayHours))))
            ScrollView(.vertical) {
                VStack(alignment: .leading, spacing: Space.s8) {
                    head
                        .onGeometryChange(for: CGFloat.self) { $0.size.height } action: { headHeight = $0 }
                    HoursGrid(
                        column: column, pph: pph, nightHeight: nightHeight,
                        selected: store.calendarMonthItem,
                        now: calendar.isDate(column.day, inSameDayAs: now) ? now : nil,
                        calendar: calendar)
                }
                .padding(.bottom, Space.scaled(20))
            }
            .scrollBounceBehavior(.basedOnSize)
            .scrollIndicators(.hidden)
            .onGeometryChange(for: CGFloat.self) { $0.size.height } action: { visibleHeight = $0 }
        }

        private var head: some View {
            VStack(alignment: .leading, spacing: Space.s4) {
                if let back {
                    BackLink(text: back) { store.calendarDayOpen = false }
                }
                HStack(alignment: .firstTextBaseline, spacing: Space.s8) {
                    Text(Agenda.format(column.day, "EEE MMM d", calendar))
                        .font(Typo.ui(15, weight: .semibold))
                        .foregroundStyle(Palette.ink)
                    if calendar.isDate(column.day, inSameDayAs: now) {
                        Text("today")
                            .font(Typo.micro)
                            .foregroundStyle(Palette.accentInk)
                    }
                    Spacer(minLength: Space.s8)
                    if !column.unlikely.isEmpty {
                        Button(action: toggleUnlikely) {
                            HStack(spacing: Space.s4) {
                                Text("\(column.unlikely.count) unlikely")
                                Image(systemName: "chevron.right")
                                    .font(Typo.ui(8, weight: .semibold))
                                    .rotationEffect(.degrees(unlikelyOpen ? 90 : 0))
                            }
                            .font(Typo.micro)
                        }
                        .buttonStyle(.textAction)
                        .pointingHand()
                    }
                }
                if unlikelyOpen {
                    VStack(alignment: .leading, spacing: 0) {
                        ForEach(column.unlikely) { item in
                            WordRow(
                                item: item,
                                lead: item.allDay ? "all day" : Agenda.clock(item.start, calendar),
                                trail: nil,
                                selected: store.calendarMonthItem == item.id)
                        }
                    }
                    .padding(.vertical, Space.s2)
                    .transition(.opacity)
                }
                if !column.allDay.isEmpty {
                    VStack(alignment: .leading, spacing: 0) {
                        ForEach(column.allDay) { item in
                            WordRow(
                                item: item,
                                lead: nil,
                                trail: Agenda.endLabel(item, on: column.day, calendar: calendar)?.text
                                    ?? (item.allDay ? "all day" : nil),
                                selected: store.calendarMonthItem == item.id)
                        }
                    }
                }
            }
        }
    }

    /// An item as words: an all-day item or a span over the hours, or an
    /// unlikely one let out of its fold.
    private struct WordRow: View {
        @Environment(AppStore.self) private var store
        let item: AgendaItem
        let lead: String?
        let trail: String?
        let selected: Bool

        var body: some View {
            ListRow(
                selected: selected, hPadding: Space.s6, vPadding: Space.scaled(3),
                action: { store.calendarMonthItem = item.id }
            ) { _, _ in
                HStack(alignment: .firstTextBaseline, spacing: Space.s8) {
                    if let lead {
                        Text(lead)
                            .font(Typo.num(11))
                            .foregroundStyle(Palette.inkFaint)
                            .frame(width: Typo.measure(44), alignment: .trailing)
                    }
                    AgendaMark(fill: item.fill, thin: item.folded || item.cancelled)
                        .frame(width: 9, height: 9)
                        .alignmentGuide(.firstTextBaseline) { $0[.bottom] - 1 }
                    Text(item.title)
                        .font(Typo.rowSub)
                        .foregroundStyle(Palette.ink)
                        .strikethrough(item.cancelled, color: Palette.inkFaint)
                        .lineLimit(1)
                    Spacer(minLength: Space.s6)
                    if let trail {
                        Text(trail)
                            .font(Typo.micro)
                            .foregroundStyle(Palette.inkFaintest)
                            .lineLimit(1)
                    }
                }
            }
            .help(item.title)
        }
    }

    /// The hours: a gutter of times, and the well they label.
    private struct HoursGrid: View {
        @Environment(AppStore.self) private var store
        let column: DayColumn
        let pph: CGFloat
        let nightHeight: CGFloat
        let selected: String?
        let now: Date?
        let calendar: Calendar

        @State private var width: CGFloat = 0

        private var gutter: CGFloat { Typo.measure(38) }
        private var from: Int { MonthGrid.dayFromHour }

        var body: some View {
            let hours = CGFloat(MonthGrid.dayHours)
            let total = nightHeight + hours * pph
            HStack(alignment: .top, spacing: Space.s6) {
                ZStack(alignment: .topTrailing) {
                    Text("night")
                        .offset(y: nightHeight / 2 - 6)
                    ForEach((from + 1)..<24, id: \.self) { h in
                        Text(hourLabel(h))
                            .offset(y: nightHeight + CGFloat(h - from) * pph - 6)
                    }
                }
                .font(Typo.num(10))
                .foregroundStyle(Palette.inkFaintest)
                .frame(width: gutter, height: total, alignment: .topTrailing)

                ZStack(alignment: .topLeading) {
                    RoundedRectangle(cornerRadius: 6, style: .continuous)
                        .fill(Palette.hairline.opacity(0.4))
                        .frame(height: nightHeight - 2)
                    ForEach(from...24, id: \.self) { h in
                        Rectangle()
                            .fill(Palette.hairline)
                            .frame(height: 0.5)
                            .offset(y: nightHeight + CGFloat(h - from) * pph)
                    }
                    ForEach(column.night) { block in
                        nightBlock(block)
                    }
                    ForEach(column.blocks) { block in
                        dayBlock(block)
                    }
                    if let now, let mark = MonthGrid.nowMark(now, day: column.day, calendar: calendar) {
                        let y = mark.part == .night
                            ? nightHeight * mark.at : nightHeight + hours * pph * mark.at
                        Rectangle()
                            .fill(Palette.accent)
                            .frame(height: 2)
                            .offset(y: y - 1)
                            .allowsHitTesting(false)
                    }
                }
                .frame(maxWidth: .infinity, alignment: .topLeading)
                .frame(height: total, alignment: .topLeading)
                .onGeometryChange(for: CGFloat.self) { $0.size.width } action: { width = $0 }
            }
        }

        private func hourLabel(_ h: Int) -> String {
            h == 12 ? "noon" : (h < 12 ? "\(h)am" : "\(h - 12)pm")
        }

        private func lane(_ block: ColumnBlock) -> (x: CGFloat, w: CGFloat) {
            let share = width / CGFloat(max(1, block.lanes))
            return (share * CGFloat(block.lane) + (block.lane > 0 ? 1 : 0), max(0, share - 2))
        }

        private func dayBlock(_ block: ColumnBlock) -> some View {
            let s = block.segment
            let span = CGFloat(MonthGrid.dayHours) * pph
            let top = nightHeight + span * s.lo
            let height = max(14, span * (s.hi - s.lo) - 2)
            let (x, w) = lane(block)
            return ColumnBlockView(
                segment: s,
                height: height,
                // The morning after a guessed end is all guess: no cap.
                capHeight: s.continuesBefore ? 0 : pph / 2,
                time: MonthGrid.rangeText(s.item, on: column.day, calendar: calendar),
                end: Agenda.endLabel(s.item, on: column.day, calendar: calendar),
                selected: selected == s.item.id
            ) { store.calendarMonthItem = s.item.id }
                .frame(width: w, height: height)
                .offset(x: x, y: top)
        }

        /// The folded night: what is there still shows, as a thin block with
        /// "until 6am" or its start, so the grid keeps its hours for the day.
        private func nightBlock(_ block: ColumnBlock) -> some View {
            let s = block.segment
            let top = nightHeight * s.lo
            let height = max(6, nightHeight * (s.hi - s.lo) - 2)
            let (x, w) = lane(block)
            let shape = UnevenRoundedRectangle(
                topLeadingRadius: s.continuesBefore ? 0 : 4, bottomLeadingRadius: s.continuesAfter ? 0 : 4,
                bottomTrailingRadius: s.continuesAfter ? 0 : 4, topTrailingRadius: s.continuesBefore ? 0 : 4,
                style: .continuous)
            let text = s.continuesBefore
                ? "until \(s.item.endEstimated ? "~" : "")\(s.item.end.map { Agenda.clock($0, calendar) } ?? "")"
                : s.item.title
            return Button {
                store.calendarMonthItem = s.item.id
            } label: {
                Text(height >= 11 ? text : "")
                    .font(Typo.ui(9.5))
                    .foregroundStyle(Palette.inkDim)
                    .lineLimit(1)
                    .padding(.horizontal, Space.s6)
                    .frame(width: w, height: height, alignment: .leading)
                    .background { BlockFill(fill: s.item.fill, thin: s.item.cancelled, shape: shape) }
                    .overlay(shape.strokeBorder(Palette.accent.opacity(selected == s.item.id ? 0.9 : 0), lineWidth: 1.5))
                    .contentShape(shape)
            }
            .buttonStyle(.plain)
            .help(s.item.title)
            .offset(x: x, y: top)
        }
    }

    /// One block in the column: its words at full strength on the band's fill.
    /// A guessed end draws the known start solid for half an hour and lets the
    /// rest thin out with no bottom edge, saying "ends ~11pm" just above
    /// where it fades.
    private struct ColumnBlockView: View {
        let segment: DaySegment
        let height: CGFloat
        /// How much of a guessed end is "the known start": half an hour, a
        /// convention and not data (the mock's open question).
        let capHeight: CGFloat
        let time: String
        let end: Agenda.EndLabel?
        let selected: Bool
        let action: () -> Void

        var body: some View {
            let item = segment.item
            let r: CGFloat = 6
            let top: CGFloat = segment.continuesBefore ? 0 : r
            let bottom: CGFloat = segment.continuesAfter ? 0 : r
            let shape = UnevenRoundedRectangle(
                topLeadingRadius: top, bottomLeadingRadius: bottom,
                bottomTrailingRadius: bottom, topTrailingRadius: top, style: .continuous)
            let tight = height < 30
            let feathered = item.endEstimated && item.fill == .solid && !item.cancelled
            let roomForEnd = item.endEstimated && end != nil && height > 56
            Button(action: action) {
                ZStack(alignment: .topLeading) {
                    fill(shape: shape, feathered: feathered)
                    words(tight: tight, showsEnd: roomForEnd)
                        .padding(.horizontal, Space.s6)
                        .padding(.top, tight ? 1 : Space.scaled(3))
                    if roomForEnd, let end {
                        EndText(label: end)
                            .padding(.leading, Space.s6)
                            .frame(maxHeight: .infinity, alignment: .bottom)
                            .padding(.bottom, Space.scaled(16))
                    }
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
                .clipShape(shape)
                // Selected: an even edge all the way round, in the state blue.
                .overlay(shape.strokeBorder(Palette.accent.opacity(selected ? 0.9 : 0), lineWidth: 1.5))
                .contentShape(shape)
            }
            .buttonStyle(.plain)
            .help(item.title)
        }

        @ViewBuilder
        private func fill(shape: UnevenRoundedRectangle, feathered: Bool) -> some View {
            if feathered {
                // Known start, guessed body, no edge: solid for the cap, the
                // same hue thinner below (still opaque: it blocks the time),
                // and feathered out over the last 20 points.
                let capStop = min(0.9, max(0, capHeight / max(1, height)))
                // Where the block runs on past midnight there is no fade:
                // the guess is still going.
                let fade = segment.continuesAfter ? 0 : 20 / max(1, height)
                shape
                    .fill(
                        LinearGradient(
                            stops: [
                                .init(color: CalendarInk.account.opacity(0.30), location: capStop),
                                .init(color: CalendarInk.account.opacity(0.14), location: min(1, capStop + 10 / max(1, height))),
                            ], startPoint: .top, endPoint: .bottom)
                    )
                    .mask(
                        LinearGradient(
                            stops: [
                                .init(color: .black, location: max(0, 1 - fade)),
                                .init(color: fade > 0 ? .clear : .black, location: 1),
                            ], startPoint: .top, endPoint: .bottom))
            } else {
                BlockFill(fill: segment.item.fill, thin: segment.item.cancelled, shape: shape)
            }
        }

        @ViewBuilder
        private func words(tight: Bool, showsEnd: Bool) -> some View {
            let item = segment.item
            let title = HStack(spacing: Space.s4) {
                // A commitment made in mail looks like a real event; the
                // envelope is what says where it came from.
                if item.isGhost, item.fill == .solid {
                    Image(systemName: "envelope")
                        .font(Typo.ui(8.5, weight: .medium))
                        .foregroundStyle(Palette.inkDim)
                }
                Text(item.title)
                    .font(Typo.ui(11, weight: item.fill == .outline ? .medium : .semibold))
                    .foregroundStyle(Palette.ink)
                    .strikethrough(item.cancelled, color: Palette.inkFaint)
            }
            .lineLimit(1)
            // With the end said at the foot, the time line says only when it
            // starts.
            let meta = showsEnd ? time.components(separatedBy: " to ").first ?? time : time
            if tight {
                HStack(alignment: .firstTextBaseline, spacing: Space.s6) {
                    title.layoutPriority(1)
                    Text(meta)
                        .font(Typo.ui(10.5))
                        .foregroundStyle(Palette.inkDim)
                        .lineLimit(1)
                }
            } else {
                VStack(alignment: .leading, spacing: 1) {
                    title
                    Text(meta)
                        .font(Typo.ui(10.5))
                        .foregroundStyle(Palette.inkDim)
                        .lineLimit(1)
                }
            }
        }
    }
#endif
