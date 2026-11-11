// THE CALENDAR PAGE'S MONTH: a conventional month grid, and the selected
// day's column beside it.
//
// WAVE 3 (the founder: "the horizontal bar on each day is confusing. lets do
// top down like gmail does. lets add more color too."):
//   - Each cell lists its day top-down, one row per thing: trips and stays
//     first, as named bars across the cells they cover (flat where they run
//     on into the next row); then the day's own all-day items; then timed
//     items by start, "7:45pm  Potluck Dinner". What does not fit folds into
//     "+N more", which opens the day.
//   - Every row is a tinted chip in its calendar's colour, brought inside the
//     app's palette by `CalendarColor` (same lightness for every calendar,
//     chroma capped, hue kept), so a month of Google colours reads as one
//     set of inks and not as confetti. Ghosts take the accent's hue and keep
//     the band language on it: solid committed, striped likely, outline
//     maybe; unlikely is a count on the day; cancelled is struck.
//   - Blue is state only: today's date and the now line. Selection is an even
//     1pt edge in ink, all the way round, on a cell or on a chip.
//
// WAVE 6: the hours (`HoursGutter`, `HoursWell`), the trip bars (`SpanBar`),
// the bar over the grid (`MonthBar`) and the footer are shared with the week
// view (Views/CalendarWeek.swift), which is why they are not private.
//
// WAVE 4:
//   - NO GAP UNDER THE DATE. A trip's bar keeps its lane only over the days
//     it covers, and a cell's own rows fill what is free above before going
//     below (`MonthDay.rows`), so every cell is laid out by row number, not
//     stacked under a block kept for the bars.
//   - THE EVENT PANE (Views/CalendarEventPane.swift) takes half the window
//     the way the composer does. While it is open the column and the detail
//     are away, the grid has the other half to itself and stays live, a
//     click on a day sets the draft's date, and the draft is on the grid as
//     a dashed chip in its calendar's colour.
//   - DRAG TO CREATE. A press and a drag down the day column's hours sweeps
//     a span, snapped to the quarter hour and labelled as it goes; letting
//     go opens the pane with it (Lib/DaySweep.swift).
//
// THE LEVELS. The month is a stack: the grid, the selected day's column, the
// selected item's detail (`CalendarDetail`, the agenda's own). A wide window
// shows the grid and one pane beside it, which is the column, or the detail
// when an item is open (with a way back to the column). A narrow one shows
// only the deepest open level, each with a way back. Escape closes them from
// the deepest (`AppStore.calendarMonthBack`).
//
// WIDTH COMES FROM THE CONTAINER. The grid sits in a GeometryReader and every
// cell is a seventh of what IT was offered. Measuring the scroll view instead
// ratcheted: a vertical ScrollView will not go narrower than its content, so
// cells sized from it grew with the window and never shrank back.
//
// All of the arithmetic is Lib/MonthGrid.swift and Lib/CalendarColor.swift,
// pure and tested; this file only draws it.

#if os(macOS)
    import SwiftUI

    // MARK: - the header switch

    /// Month, week or agenda, in the page header, month first. No Day: the
    /// day is the column beside the month, and one of the week's seven.
    struct CalendarModeSwitch: View {
        @Environment(AppStore.self) private var store

        var body: some View {
            GlassSegmented(
                options: CalendarViewMode.pickerOrder.map { (value: $0, label: $0.label) },
                selection: Binding(
                    get: { store.calendarMode },
                    set: { store.setCalendarMode($0) }))
        }
    }

    // MARK: - colours

    // MARK: - metrics

    /// The month's measurements, in one place so the cell, the span layer and
    /// the fold agree. Computed, never stored: they follow the density setting
    /// (`Space.scaled`).
    @MainActor
    enum MonthMetrics {
        /// The date's band at the top of a cell.
        static var numberBand: CGFloat { Space.scaled(26) }
        static var rowHeight: CGFloat { Space.scaled(17) }
        static var rowGap: CGFloat { 2 }
        static var step: CGFloat { rowHeight + rowGap }
        /// Room kept under the last row, so a cell never ends on a chip.
        static var foot: CGFloat { Space.s4 }
        /// A chip's inset from its cell's sides.
        static var inset: CGFloat { 3 }
        /// The narrowest row: the date, two rows, the foot. Past it the grid
        /// scrolls rather than folding everything.
        static func minRow(lanes: Int) -> CGFloat { numberBand + CGFloat(lanes + 2) * step + foot }
        static var timeMinWidth: Double { Double(Space.scaled(CGFloat(MonthGrid.rowTimeMinWidth))) }
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
        /// The event pane is open beside the page: the grid alone, whatever
        /// levels were open, until it closes.
        let drafting: Bool
        let paneWidth: CGFloat
        let unlikelyOpen: Bool
        let toggleUnlikely: () -> Void
        let now: Date
        let calendar: Calendar

        var body: some View {
            VStack(spacing: 0) {
                MonthBar(title: MonthGrid.title(place.month, calendar: calendar), day: place.day, unit: .month)
                HStack(alignment: .top, spacing: Space.scaled(18)) {
                    if drafting || twoPanes || (item == nil && !store.calendarDayOpen) {
                        MonthGridView(
                            model: model, place: place, item: drafting ? nil : item, twoPanes: twoPanes,
                            now: now, calendar: calendar)
                    }
                    if !drafting, twoPanes || item != nil || store.calendarDayOpen {
                        pane
                            .frame(width: twoPanes ? paneWidth : nil)
                            .frame(maxWidth: twoPanes ? paneWidth : .infinity, maxHeight: .infinity, alignment: .top)
                    }
                }
                .padding(.horizontal, Space.page)
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
    struct BackLink: View {
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

    /// The month's own bar, and the week's: which month (or week), the way
    /// to the ones beside it, back to today, and a new event on the selected
    /// day. The page header above it carries the mode switch.
    struct MonthBar: View {
        @Environment(AppStore.self) private var store
        let title: String
        let day: Date
        let unit: Unit

        /// What the arrows turn.
        enum Unit {
            case month
            case week

            var word: String { self == .month ? "month" : "week" }
            func move(_ n: Int) -> MonthMove { self == .month ? .months(n) : .days(7 * n) }
        }

        var body: some View {
            HStack(spacing: Space.s8) {
                ChromeChip(icon: "chevron.left", help: "Previous \(unit.word) ( [ )") {
                    store.moveCalendarMonth(unit.move(-1))
                }
                ChromeChip(icon: "chevron.right", help: "Next \(unit.word) ( ] )") {
                    store.moveCalendarMonth(unit.move(1))
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
                // Only where the daemon lists a calendar the user can write
                // to: an older one never shows the control at all.
                // Not while the pane it opens is already open.
                if store.calendarCanCreate, !store.eventPaneOpen {
                    ChromeChip(text: "New event", icon: "plus", tone: Palette.ink, help: "New event ( n )") {
                        store.openEventEditor(day: day)
                    }
                }
            }
            .padding(.horizontal, Space.page)
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

        /// The rows' visible height. Only the HEIGHT is measured off the
        /// scroll view: it is offered a height and takes it, whatever its
        /// content. Its width is not trustworthy (see the file's header).
        @State private var visibleHeight: CGFloat = 0
        /// Days being swept (drag to create): the cell pressed and the cell
        /// under the pointer (`DaysSweep`). Nil when there is none.
        @State private var sweep: Sweep?
        /// Escape was pressed mid-drag: the rest of this drag is ignored.
        @State private var sweepCancelled = false

        private struct Sweep: Equatable {
            var from: Int
            var to: Int
        }

        var body: some View {
            GeometryReader { geo in
                let width = geo.size.width
                let cellWidth = CGFloat(MonthGrid.cellWidth(gridWidth: Double(width)))
                let rows = max(1, model.weeks.count)
                // The floor makes room for two bars; a third and more are
                // drawn when the cells are tall enough (`MonthGrid.fit`).
                let busiest = min(MonthGrid.floorLanes, model.weeks.map(\.lanes).max() ?? 0)
                // The rows share the height there is, and never go below what
                // a cell needs; past that the grid scrolls.
                let rowHeight = max(
                    MonthMetrics.minRow(lanes: busiest), floor(visibleHeight / CGFloat(rows)))
                VStack(alignment: .leading, spacing: 0) {
                    headings(cellWidth: cellWidth)
                    ScrollView(.vertical) {
                        VStack(spacing: 0) {
                            ForEach(model.weeks) { week in
                                MonthWeekRow(
                                    week: week, month: model.month, place: place, item: item,
                                    twoPanes: twoPanes, cellWidth: cellWidth, height: rowHeight,
                                    now: now, calendar: calendar)
                            }
                        }
                        .frame(width: width, alignment: .topLeading)
                        .overlay(alignment: .topLeading) {
                            if let sweep {
                                sweptRun(sweep, cellWidth: cellWidth, rowHeight: rowHeight)
                            }
                        }
                        .coordinateSpace(name: Self.rowsSpace)
                        // The page turned mid-drag: the cells it held are gone.
                        .onChange(of: model.month) { _, _ in
                            if sweep != nil { sweepCancelled = true }
                            sweep = nil
                        }
                        .simultaneousGesture(sweepGesture(cellWidth: cellWidth, rowHeight: rowHeight))
                    }
                    .scrollBounceBehavior(.basedOnSize)
                    .scrollIndicators(.hidden)
                    .frame(width: width)
                    .frame(maxHeight: .infinity)
                    .onGeometryChange(for: CGFloat.self) { $0.size.height } action: { visibleHeight = $0 }
                    MonthFooter(
                        loading: store.calendarFeed == nil && !store.calendarLoaded, canCreate: store.calendarCanCreate,
                        drafting: store.eventPaneOpen)
                }
                .frame(width: width, height: geo.size.height, alignment: .topLeading)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .accessibilityElement(children: .contain)
            .accessibilityLabel(MonthGrid.title(model.month, calendar: calendar))
        }

        // MARK: drag across days to create

        private static let rowsSpace = "monthRows"
        private var days: [Date] { model.weeks.flatMap { $0.days.map(\.day) } }
        /// Only where a new event can be made at all, as `n` is.
        private var canSweep: Bool { store.calendarCanCreate && !store.eventPaneOpen }

        private func cell(_ p: CGPoint, cellWidth: CGFloat, rowHeight: CGFloat) -> Int {
            DaysSweep.cell(
                x: Double(p.x), y: Double(p.y), cellWidth: Double(cellWidth), rowHeight: Double(rowHeight),
                rows: model.weeks.count)
        }

        private func sweepGesture(cellWidth: CGFloat, rowHeight: CGFloat) -> some Gesture {
            DragGesture(minimumDistance: CGFloat(DaySweep.clickSlop), coordinateSpace: .named(Self.rowsSpace))
                .onChanged { value in
                    guard canSweep, !sweepCancelled else { return }
                    let from = cell(value.startLocation, cellWidth: cellWidth, rowHeight: rowHeight)
                    let to = cell(value.location, cellWidth: cellWidth, rowHeight: rowHeight)
                    if sweep == nil {
                        // Not until it has left the day it was pressed on: a
                        // wobble inside one cell is still a click.
                        guard from != to else { return }
                        store.calendarSweepCancel = {
                            sweepCancelled = true
                            sweep = nil
                        }
                    }
                    sweep = Sweep(from: from, to: to)
                }
                .onEnded { _ in
                    let swept = sweepCancelled ? nil : sweep
                    sweep = nil
                    sweepCancelled = false
                    store.calendarSweepCancel = nil
                    let all = days
                    guard let swept, canSweep, all.indices.contains(swept.from), all.indices.contains(swept.to)
                    else { return }
                    let first = all[min(swept.from, swept.to)]
                    let last = all[max(swept.from, swept.to)]
                    // After the click the release may also be (the cell's
                    // own tap), so the pane opens last and keeps the run.
                    DispatchQueue.main.async {
                        store.selectCalendarDay(first, open: false)
                        store.openEventEditor(from: first, through: last)
                    }
                }
        }

        /// The run being swept: a bar like any other across its days, dashed
        /// in the colour of the calendar a new event goes on, under the bars
        /// each row already has.
        private func sweptRun(_ sweep: Sweep, cellWidth: CGFloat, rowHeight: CGFloat) -> some View {
            // Clamped both ends: the grid may have fewer cells than when the
            // drag began (the page turned under it, which also drops it).
            let all = days
            let last = max(0, all.count - 1)
            let lo = min(last, max(0, min(sweep.from, sweep.to)))
            let hi = min(last, max(lo, max(sweep.from, sweep.to)))
            let hue = CalendarColor.parse(store.calendarWritable.first?.color).map(ChipHue.calendar) ?? .plain
            let inks = ChipColors.inks(hue)
            let inset = MonthMetrics.inset
            let fits = max(1, Int((rowHeight - MonthMetrics.numberBand - MonthMetrics.foot) / MonthMetrics.step))
            let when = hi > lo && !all.isEmpty
                ? "\(Agenda.format(all[lo], "MMM d", calendar)) – \(Agenda.format(all[hi], "MMM d", calendar))"
                : all.isEmpty ? "" : Agenda.format(all[lo], "MMM d", calendar)
            return ZStack(alignment: .topLeading) {
                ForEach(DaysSweep.pieces(from: lo, to: hi), id: \.row) { piece in
                    let lanes = model.weeks.indices.contains(piece.row) ? model.weeks[piece.row].lanes : 0
                    let lane = min(lanes, fits - 1)
                    let cells = CGFloat(piece.last - piece.first + 1)
                    let lead: CGFloat = piece.startsHere ? inset : 0
                    let width = max(0, cells * cellWidth - lead - (piece.endsHere ? inset : 0))
                    let r: CGFloat = 4
                    let shape = UnevenRoundedRectangle(
                        topLeadingRadius: piece.startsHere ? r : 0, bottomLeadingRadius: piece.startsHere ? r : 0,
                        bottomTrailingRadius: piece.endsHere ? r : 0, topTrailingRadius: piece.endsHere ? r : 0,
                        style: .continuous)
                    // The days themselves, washed, so the run reads at a glance.
                    Rectangle()
                        .fill(Palette.accentSoft.opacity(0.6))
                        .frame(width: cells * cellWidth, height: rowHeight)
                        .offset(x: CGFloat(piece.first) * cellWidth, y: CGFloat(piece.row) * rowHeight)
                    HStack(alignment: .firstTextBaseline, spacing: Space.s6) {
                        if piece.startsHere {
                            Text("New event")
                                .font(Typo.ui(10.5, weight: .semibold))
                                .foregroundStyle(Palette.ink)
                            Text(when)
                                .font(Typo.num(10))
                                .foregroundStyle(inks.ink)
                        }
                    }
                    .lineLimit(1)
                    .padding(.horizontal, Space.s6)
                    .frame(width: width, height: MonthMetrics.rowHeight, alignment: .leading)
                    .background {
                        ChipFill(
                            style: ChipStyle(hue: hue, fill: .outline, thin: false, struck: false), inks: inks,
                            shape: shape, provisional: true)
                    }
                    .clipShape(shape)
                    .offset(
                        x: CGFloat(piece.first) * cellWidth + lead,
                        y: CGFloat(piece.row) * rowHeight + MonthMetrics.numberBand + CGFloat(lane) * MonthMetrics.step)
                }
            }
            .allowsHitTesting(false)
            .accessibilityLabel("New event, \(when)")
        }

        private func headings(cellWidth: CGFloat) -> some View {
            let labels = MonthGrid.weekdayLabels(calendar)
            return HStack(spacing: 0) {
                ForEach(labels.indices, id: \.self) { i in
                    Text(labels[i])
                        .font(Typo.ui(10, weight: .medium))
                        .foregroundStyle(Palette.inkFaintest)
                        .lineLimit(1)
                        .padding(.leading, Space.s8)
                        .frame(width: cellWidth, alignment: .leading)
                }
            }
            .padding(.bottom, Space.s6)
        }
    }

    /// Loading, and the keys.
    struct MonthFooter: View {
        let loading: Bool
        let canCreate: Bool
        /// The event pane is open: the grid's one job beside it is said.
        let drafting: Bool
        /// The week's footer: its arrows move the day and walk its items.
        var week = false

        var body: some View {
            HStack(spacing: Space.s12) {
                if loading {
                    Text("loading your calendar…")
                        .font(Typo.micro)
                        .foregroundStyle(Palette.inkFaintest)
                        .lineLimit(1)
                }
                Spacer(minLength: Space.s8)
                if drafting {
                    Text("Click a day to move the event there")
                        .font(Typo.micro)
                        .foregroundStyle(Palette.inkFaintest)
                        .lineLimit(1)
                } else {
                    if canCreate {
                        KeyHint("n", "new event")
                    }
                    if week {
                        KeyHint(["←", "→"], "day")
                        KeyHint(["j", "k"], "items")
                    } else {
                        KeyHint(["←", "→", "↑", "↓"], "move")
                    }
                    KeyHint("↵", "open")
                }
            }
            .padding(.vertical, Space.s10)
        }
    }

    // MARK: - a week row

    private struct MonthWeekRow: View {
        @Environment(AppStore.self) private var store
        let week: MonthWeek
        let month: Date
        let place: MonthPlace
        let item: AgendaItem?
        let twoPanes: Bool
        let cellWidth: CGFloat
        let height: CGFloat
        let now: Date
        let calendar: Calendar

        var body: some View {
            // A bar is one of a cell's rows now, so no rows are kept back for
            // the lanes: each cell counts around the bars over its own day.
            let slots = MonthGrid.slots(
                cellHeight: Double(height), lanes: 0,
                numberBand: Double(MonthMetrics.numberBand), rowHeight: Double(MonthMetrics.rowHeight),
                rowGap: Double(MonthMetrics.rowGap), foot: Double(MonthMetrics.foot))
            // The pane's draft is being placed: a click anywhere in a cell,
            // on a chip or beside it, is that day.
            let drafting = store.eventPaneOpen
            // The bars that fit at this height, and each day's rows around them.
            let fit = MonthGrid.fit(week, slots: slots)
            ZStack(alignment: .topLeading) {
                HStack(spacing: 0) {
                    ForEach(Array(week.days.enumerated()), id: \.element.id) { i, day in
                        MonthCell(
                            day: day,
                            label: MonthGrid.cellLabel(day.day, month: month, calendar: calendar),
                            selected: day.day == place.day,
                            today: calendar.isDate(day.day, inSameDayAs: now),
                            rows: fit.rows[i],
                            fold: MonthGrid.fold(
                                rows: fit.rows[i],
                                barLanes: MonthGrid.barLanes(fit.bars, column: i),
                                hiddenSpans: fit.overflow[i],
                                slots: slots),
                            selectedItem: item?.id,
                            drafting: drafting,
                            width: cellWidth,
                            now: now,
                            calendar: calendar,
                            open: { store.selectCalendarDay(day.day, open: !twoPanes) },
                            pick: { picked in store.selectCalendarItem(picked.id, on: day.day) })
                    }
                }
                ForEach(fit.bars) { span in
                    SpanBar(
                        segment: span,
                        label: MonthGrid.spanLabel(span, calendar: calendar),
                        selected: item?.id == span.item.id,
                        cellWidth: cellWidth
                    ) {
                        // The bar's own first day in this row: a click lands
                        // the selection where the user is looking.
                        let first = week.days[min(span.firstColumn, week.days.count - 1)].day
                        store.selectCalendarItem(span.item.id, on: max(first, calendar.startOfDay(for: span.item.start)))
                    }
                    .offset(
                        x: CGFloat(span.firstColumn) * cellWidth + MonthMetrics.inset,
                        y: MonthMetrics.numberBand + CGFloat(span.lane) * MonthMetrics.step)
                    // Through to the cell under the pointer while drafting,
                    // so the click lands on the day it was aimed at.
                    .allowsHitTesting(!drafting)
                }
            }
            .frame(width: cellWidth * 7, height: height, alignment: .topLeading)
            .overlay(alignment: .top) { Hairline() }
        }
    }

    // MARK: - a cell

    private struct MonthCell: View {
        let day: MonthDay
        let label: String
        let selected: Bool
        let today: Bool
        /// The row each of the day's items takes, around the bars drawn at
        /// this height (`MonthGrid.fit`).
        let rows: [Int]
        let fold: CellFold
        let selectedItem: String?
        /// The event pane is open: chips do not take the click, the cell does.
        let drafting: Bool
        let width: CGFloat
        let now: Date
        let calendar: Calendar
        let open: () -> Void
        let pick: (AgendaItem) -> Void

        /// Where row `n` under the date starts. The week row's bars use the
        /// same sum, so a bar and the rows around it share one grid.
        private func top(_ row: Int) -> CGFloat {
            MonthMetrics.numberBand + CGFloat(row) * MonthMetrics.step
        }

        var body: some View {
            let shape = RoundedRectangle(cornerRadius: Radius.row, style: .continuous)
            let shown = Array(day.items.prefix(fold.shown))
            let nowAt = today ? MonthGrid.nowSlot(day.items, now: now) : nil
            // EVERY ROW AT ITS OWN NUMBER, not stacked: the day's items take
            // the rows no bar covers (`MonthDay.rows`), which may be above a
            // bar as well as below it, so the first thing in a cell is
            // directly under its date whenever there is anything to put there.
            ZStack(alignment: .topLeading) {
                numberRow
                    .frame(height: MonthMetrics.numberBand - MonthMetrics.rowGap, alignment: .center)
                ForEach(Array(shown.enumerated()), id: \.element.id) { i, item in
                    CellRow(
                        item: item,
                        time: MonthGrid.cellTime(
                            item, cellWidth: Double(width), minWidth: MonthMetrics.timeMinWidth,
                            calendar: calendar),
                        selected: selectedItem == item.id,
                        width: width - 2 * MonthMetrics.inset
                    ) { pick(item) }
                    .allowsHitTesting(!drafting)
                    // The now line, in the gap above the first row still to
                    // come (or under the last when every row has begun).
                    .overlay(alignment: .top) {
                        if nowAt == i { NowRule().offset(y: -MonthMetrics.rowGap / 2 - 0.75) }
                    }
                    .overlay(alignment: .bottom) {
                        if nowAt == shown.count, i == shown.count - 1 {
                            NowRule().offset(y: MonthMetrics.rowGap / 2 + 0.75)
                        }
                    }
                    .offset(y: top(i < rows.count ? rows[i] : i))
                }
                if fold.more > 0, let moreRow = fold.moreRow {
                    Button(action: open) {
                        Text("+\(fold.more) more")
                            .font(Typo.ui(10, weight: .medium))
                            .padding(.horizontal, Space.s4)
                            .frame(height: MonthMetrics.rowHeight)
                    }
                    .buttonStyle(.textAction)
                    .pointingHand()
                    .help("Open \(Agenda.format(day.day, "EEE MMM d", calendar))")
                    .allowsHitTesting(!drafting)
                    .offset(y: top(moreRow))
                }
            }
            // The stack is the whole cell, so a row moved down by its number
            // is still inside what it is drawn and hit in.
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
            .padding(.horizontal, MonthMetrics.inset)
            .frame(width: width, alignment: .topLeading)
            .frame(maxHeight: .infinity, alignment: .top)
            .clipped()
            .opacity(day.inMonth ? 1 : 0.55)
            // Selected: an EVEN 1pt edge all the way round, in ink, not blue.
            .overlay(shape.strokeBorder(Palette.inkFaint.opacity(selected ? 0.75 : 0), lineWidth: 1).padding(1))
            .contentShape(Rectangle())
            .onTapGesture(perform: open)
            .accessibilityElement(children: .contain)
            .accessibilityLabel(accessibility)
            .accessibilityAddTraits(selected ? [.isButton, .isSelected] : .isButton)
            .accessibilityAction(.default, open)
        }

        private var numberRow: some View {
            HStack(alignment: .center, spacing: Space.s4) {
                Text(label)
                    .font(Typo.num(11.5, weight: today ? .bold : .medium))
                    .foregroundStyle(today ? Color.white : (day.inMonth ? Palette.inkDim : Palette.inkFaintest))
                    .lineLimit(1)
                    // Every date sits the same distance in: today's capsule
                    // pads by exactly the inset the others are given, so its
                    // digits line up with the column under it.
                    .padding(.horizontal, Space.s4)
                    .padding(.vertical, 1.5)
                    // Today: the date on a blue capsule, the one place in the
                    // grid the accent is a fill.
                    .background(Capsule().fill(Palette.accent.opacity(today ? 1 : 0)))
                Spacer(minLength: 0)
                if !day.unlikely.isEmpty {
                    Text("\(day.unlikely.count) unlikely")
                        .font(Typo.ui(9.5, weight: .medium))
                        .foregroundStyle(Palette.inkFaintest)
                        .lineLimit(1)
                        .layoutPriority(-1)
                        .help("\(day.unlikely.count) unlikely \(day.unlikely.count == 1 ? "plan" : "plans") from your mail; open the day to see")
                }
            }
            .padding(.trailing, Space.s2)
        }

        private var accessibility: String {
            var parts = [Agenda.format(day.day, "EEEE MMMM d", calendar)]
            if today { parts.append("today") }
            if !day.items.isEmpty { parts.append("\(day.items.count) \(day.items.count == 1 ? "item" : "items")") }
            if fold.more > 0 { parts.append("\(fold.more) more") }
            if !day.unlikely.isEmpty { parts.append("\(day.unlikely.count) unlikely") }
            return parts.joined(separator: ", ")
        }
    }

    /// Now, in today's list: a blue rule across the gap between what has
    /// started and what has not.
    private struct NowRule: View {
        var body: some View {
            Capsule()
                .fill(Palette.accent)
                .frame(height: 1.5)
                .allowsHitTesting(false)
        }
    }

    /// One row in a cell: a chip in the row's colour and fill, the time in the
    /// hue's ink, the title at full strength whatever the fill. The title is
    /// what truncates; the time never does.
    private struct CellRow: View {
        let item: AgendaItem
        let time: String?
        let selected: Bool
        let width: CGFloat
        let action: () -> Void

        var body: some View {
            let style = CalendarColor.style(item)
            let inks = ChipColors.inks(style.hue)
            let shape = RoundedRectangle(cornerRadius: 4, style: .continuous)
            // The event pane's draft, where its fields would put it.
            let draft = item.id == EventForm.provisionalId
            Button(action: action) {
                HStack(alignment: .firstTextBaseline, spacing: Space.s4) {
                    if let time {
                        Text(time)
                            .font(Typo.num(10))
                            .foregroundStyle(inks.ink)
                            .fixedSize()
                    }
                    // A commitment made in mail looks like a real event; the
                    // envelope is what says where it came from.
                    if item.isGhost, item.fill == .solid, !item.cancelled {
                        Image(systemName: "envelope")
                            .font(Typo.ui(8, weight: .medium))
                            .foregroundStyle(inks.ink)
                    }
                    Text(item.title)
                        .font(
                            Typo.ui(
                                10.5, weight: !draft && (item.fill == .outline || style.thin) ? .regular : .medium))
                        .foregroundStyle(Palette.ink)
                        .strikethrough(style.struck, color: Palette.inkFaint)
                        .lineLimit(1)
                        .truncationMode(.tail)
                }
                .padding(.horizontal, Space.s4)
                .frame(width: max(0, width), height: MonthMetrics.rowHeight, alignment: .leading)
                .background {
                    ChipFill(style: style, inks: inks, shape: shape, stripeSpacing: 5, provisional: draft)
                }
                .overlay(shape.strokeBorder(Palette.ink.opacity(selected ? 0.7 : 0), lineWidth: 1))
                .contentShape(shape)
            }
            .buttonStyle(.plain)
            .help(helpText)
        }

        private var helpText: String {
            var parts = [item.title]
            if item.cancelled { parts.append("cancelled") }
            if let band = item.band { parts.append(band.rawValue) }
            return parts.joined(separator: ", ")
        }
    }

    // MARK: - a span

    /// A trip or a stay across the top of the cells it covers, in its colour
    /// and fill: rounded where it really begins and ends, flat where it runs
    /// on into the row before or after. Its words are at full strength
    /// whatever the fill: a ghost's container thins, never its ink.
    struct SpanBar: View {
        let segment: SpanSegment
        let label: (title: String, sub: String?)
        let selected: Bool
        let cellWidth: CGFloat
        let action: () -> Void

        var body: some View {
            let item = segment.item
            let style = CalendarColor.style(item)
            let inks = ChipColors.inks(style.hue)
            let inset = MonthMetrics.inset
            let cells = CGFloat(segment.lastColumn - segment.firstColumn + 1)
            // Flat ends reach the cell's edge, so a span that runs on meets
            // its continuation across the row break's gutter, not short of it.
            let lead: CGFloat = segment.startsHere ? 0 : -inset
            let width = max(0, cells * cellWidth - 2 * inset - lead + (segment.endsHere ? 0 : inset))
            let r: CGFloat = 4
            let shape = UnevenRoundedRectangle(
                topLeadingRadius: segment.startsHere ? r : 0, bottomLeadingRadius: segment.startsHere ? r : 0,
                bottomTrailingRadius: segment.endsHere ? r : 0, topTrailingRadius: segment.endsHere ? r : 0,
                style: .continuous)
            Button(action: action) {
                HStack(alignment: .firstTextBaseline, spacing: Space.s6) {
                    if !segment.startsHere {
                        Image(systemName: "arrow.left")
                            .font(Typo.ui(7.5, weight: .semibold))
                            .foregroundStyle(inks.ink)
                    }
                    Text(label.title)
                        .font(Typo.ui(10.5, weight: .semibold))
                        .foregroundStyle(Palette.ink)
                        .strikethrough(style.struck, color: Palette.inkFaint)
                    if let sub = label.sub {
                        Text(sub)
                            .font(Typo.num(10))
                            .foregroundStyle(inks.ink)
                    }
                }
                .lineLimit(1)
                .padding(.horizontal, Space.s6)
                .frame(width: width, height: MonthMetrics.rowHeight, alignment: .leading)
                .background {
                    ChipFill(
                        style: style, inks: inks, shape: shape, stripeSpacing: 5,
                        provisional: item.id == EventForm.provisionalId)
                }
                .overlay(shape.strokeBorder(Palette.ink.opacity(selected ? 0.7 : 0), lineWidth: 1))
                .clipShape(shape)
                .contentShape(shape)
            }
            .buttonStyle(.plain)
            .pointingHand()
            .help(item.title)
            .offset(x: lead)
        }
    }

    // MARK: - the day column

    /// The selected day, hours down the side: the night folded into a thin
    /// row at the top, each block in its calendar's colour (ghosts in theirs,
    /// by band), guessed ends soft, all-day items as words above, and the
    /// unlikely folded into a count.
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
                .padding(.bottom, Space.s20)
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
            let style = CalendarColor.style(item)
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
                    AgendaMark(fill: item.fill, thin: style.thin, hue: ChipColors.inks(style.hue).edge)
                        .frame(width: 9, height: 9)
                        .alignmentGuide(.firstTextBaseline) { $0[.bottom] - 1 }
                    Text(item.title)
                        .font(Typo.rowSub)
                        .foregroundStyle(Palette.ink)
                        .strikethrough(style.struck, color: Palette.inkFaint)
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

    /// The hours: a gutter of times, and the well they label. The month's
    /// day column is one of each; the week (Views/CalendarWeek.swift) is one
    /// gutter and seven wells.
    private struct HoursGrid: View {
        @Environment(AppStore.self) private var store
        let column: DayColumn
        let pph: CGFloat
        let nightHeight: CGFloat
        let selected: String?
        let now: Date?
        let calendar: Calendar

        var body: some View {
            HStack(alignment: .top, spacing: Space.s6) {
                HoursGutter(pph: pph, nightHeight: nightHeight)
                HoursWell(
                    column: column, pph: pph, nightHeight: nightHeight, selected: selected, now: now,
                    calendar: calendar
                ) { store.calendarMonthItem = $0.id }
            }
        }
    }

    /// The times down the side of the hours: "night" on the folded cap, then
    /// each hour on its line.
    struct HoursGutter: View {
        let pph: CGFloat
        let nightHeight: CGFloat

        @MainActor static var width: CGFloat { Typo.measure(38) }

        var body: some View {
            let from = MonthGrid.dayFromHour
            let total = nightHeight + CGFloat(MonthGrid.dayHours) * pph
            ZStack(alignment: .topTrailing) {
                Text("night")
                    .offset(y: nightHeight / 2 - 6)
                ForEach((from + 1)..<24, id: \.self) { h in
                    Text(h == 12 ? "noon" : (h < 12 ? "\(h)am" : "\(h - 12)pm"))
                        .offset(y: nightHeight + CGFloat(h - from) * pph - 6)
                }
            }
            .font(Typo.num(10))
            .foregroundStyle(Palette.inkFaintest)
            .frame(width: Self.width, height: total, alignment: .topTrailing)
        }
    }

    /// One day's well under the hours: the night's cap, a line per hour, the
    /// day's blocks side by side where they overlap, and now.
    ///
    /// DRAG TO CREATE. A press on the empty well and a drag down (or up) it
    /// sweeps a span of time: a dashed block follows the pointer, snapped to
    /// the quarter hour, saying its times as it goes; letting go opens the
    /// event pane with that day and those times. The arithmetic is
    /// Lib/DaySweep.swift. A plain click on the well is `tap` (nothing, on
    /// the month's column), a press on a block is that block's click, and
    /// Escape mid-drag drops the sweep (`AppStore.calendarMonthBack`).
    struct HoursWell: View {
        @Environment(AppStore.self) private var store
        let column: DayColumn
        let pph: CGFloat
        let nightHeight: CGFloat
        let selected: String?
        let now: Date?
        let calendar: Calendar
        /// The week draws one cap across all seven days, under the wells.
        var nightCap = true
        /// A narrow column (the week's): a block is its title alone.
        var showsTime = true
        /// A plain click on the empty well: the week selects the day.
        var tap: (() -> Void)?
        /// The event pane is open beside the page: the blocks do not take
        /// the click, the well does (the day it was aimed at).
        var drafting = false
        let pick: (AgendaItem) -> Void

        @State private var width: CGFloat = 0
        /// The sweep under way: where the press landed and where the pointer
        /// is, in the well's own points. Nil when there is none.
        @State private var sweep: Sweep?
        /// Escape was pressed mid-drag: the rest of this drag is ignored.
        @State private var sweepCancelled = false

        private struct Sweep: Equatable {
            var from: CGFloat
            var to: CGFloat
        }

        private var from: Int { MonthGrid.dayFromHour }
        private var scale: DaySweepScale {
            DaySweepScale(nightHeight: Double(nightHeight), pointsPerHour: Double(pph), fromHour: from)
        }

        var body: some View {
            let hours = CGFloat(MonthGrid.dayHours)
            let total = nightHeight + hours * pph
            ZStack(alignment: .topLeading) {
                // The empty well itself, UNDER everything drawn on it: a
                // block above it keeps its own click, and a press that
                // starts on one never starts a sweep.
                Color.clear
                    .frame(maxWidth: .infinity)
                    .frame(height: total)
                    .contentShape(Rectangle())
                    .gesture(sweepGesture)
                    .onTapGesture { tap?() }
                if nightCap {
                    RoundedRectangle(cornerRadius: 6, style: .continuous)
                        .fill(Palette.hairline.opacity(0.4))
                        .frame(height: nightHeight - 2)
                        .allowsHitTesting(false)
                }
                ForEach(from...24, id: \.self) { h in
                    Rectangle()
                        .fill(Palette.hairline)
                        .frame(height: 0.5)
                        .offset(y: nightHeight + CGFloat(h - from) * pph)
                        .allowsHitTesting(false)
                }
                ForEach(column.night) { block in
                    nightBlock(block)
                        .allowsHitTesting(!drafting)
                }
                ForEach(column.blocks) { block in
                    dayBlock(block)
                        .allowsHitTesting(!drafting)
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
                if let sweep {
                    sweptBlock(sweep)
                }
            }
            // The well's width comes from the pane, never from its blocks.
            .frame(maxWidth: .infinity, alignment: .topLeading)
            .frame(height: total, alignment: .topLeading)
            .onGeometryChange(for: CGFloat.self) { $0.size.width } action: { width = $0 }
        }

        // MARK: drag to create

        /// Only where a new event can be made at all, as `n` is.
        private var canSweep: Bool { store.calendarCanCreate && !store.eventPaneOpen }

        private var sweepGesture: some Gesture {
            DragGesture(minimumDistance: CGFloat(DaySweep.clickSlop), coordinateSpace: .local)
                .onChanged { value in
                    guard canSweep, !sweepCancelled else { return }
                    if sweep == nil {
                        // Not until it has travelled DOWN or UP: a sideways
                        // wobble on a click is still a click.
                        guard
                            DaySweep.isDrag(from: Double(value.startLocation.y), to: Double(value.location.y))
                        else { return }
                        store.calendarSweepCancel = {
                            sweepCancelled = true
                            sweep = nil
                        }
                    }
                    sweep = Sweep(from: value.startLocation.y, to: value.location.y)
                }
                .onEnded { value in
                    let swept = sweepCancelled ? nil : sweep
                    sweep = nil
                    sweepCancelled = false
                    store.calendarSweepCancel = nil
                    guard let swept, canSweep else { return }
                    let span = DaySweep.span(from: Double(swept.from), to: Double(value.location.y), scale)
                    store.openEventEditor(day: column.day, startMinutes: span.start, endMinutes: span.end)
                }
        }

        /// The span being swept: the draft's own dashed chip, in the colour
        /// of the calendar a new event goes on, saying its times.
        private func sweptBlock(_ sweep: Sweep) -> some View {
            let span = DaySweep.span(from: Double(sweep.from), to: Double(sweep.to), scale)
            let top = CGFloat(DaySweep.y(of: span.start, scale))
            let height = max(14, CGFloat(DaySweep.y(of: span.end, scale)) - top - 2)
            let hue = CalendarColor.parse(store.calendarWritable.first?.color).map(ChipHue.calendar) ?? .plain
            let inks = ChipColors.inks(hue)
            let shape = RoundedRectangle(cornerRadius: Radius.chip, style: .continuous)
            return Text(DaySweep.label(span))
                .font(Typo.ui(10.5, weight: .medium))
                .monospacedDigit()
                .foregroundStyle(inks.ink)
                .lineLimit(1)
                .padding(.horizontal, Space.s6)
                .padding(.top, height < 30 ? 1 : Space.scaled(3))
                .frame(width: max(0, width - 2), height: height, alignment: .topLeading)
                .background {
                    ChipFill(
                        style: ChipStyle(hue: hue, fill: .outline, thin: false, struck: false), inks: inks,
                        shape: shape, provisional: true)
                }
                .offset(y: top)
                .allowsHitTesting(false)
                .accessibilityLabel("New event, \(DaySweep.label(span))")
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
                selected: selected == s.item.id,
                showsTime: showsTime
            ) { pick(s.item) }
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
            let style = CalendarColor.style(s.item)
            let shape = UnevenRoundedRectangle(
                topLeadingRadius: s.continuesBefore ? 0 : 4, bottomLeadingRadius: s.continuesAfter ? 0 : 4,
                bottomTrailingRadius: s.continuesAfter ? 0 : 4, topTrailingRadius: s.continuesBefore ? 0 : 4,
                style: .continuous)
            let text = s.continuesBefore
                ? "until \(s.item.endEstimated ? "~" : "")\(s.item.end.map { Agenda.clock($0, calendar) } ?? "")"
                : s.item.title
            return Button {
                pick(s.item)
            } label: {
                Text(height >= 11 ? text : "")
                    .font(Typo.ui(9.5))
                    .foregroundStyle(Palette.ink)
                    .lineLimit(1)
                    .padding(.horizontal, Space.s6)
                    .frame(width: w, height: height, alignment: .leading)
                    .background {
                        ChipFill(
                            style: style, inks: ChipColors.inks(style.hue), shape: shape,
                            provisional: s.item.id == EventForm.provisionalId)
                    }
                    .overlay(shape.strokeBorder(Palette.ink.opacity(selected == s.item.id ? 0.7 : 0), lineWidth: 1))
                    .contentShape(shape)
            }
            .buttonStyle(.plain)
            .help(s.item.title)
            .offset(x: x, y: top)
        }
    }

    /// One block in the column: its words at full strength on its colour and
    /// fill. A guessed end draws the known start solid for half an hour and
    /// lets the rest thin out with no bottom edge, saying "ends ~11pm" just
    /// above where it fades.
    private struct ColumnBlockView: View {
        let segment: DaySegment
        let height: CGFloat
        /// How much of a guessed end is "the known start": half an hour, a
        /// convention and not data (the mock's open question).
        let capHeight: CGFloat
        let time: String
        let end: Agenda.EndLabel?
        let selected: Bool
        /// False in a narrow column: the title alone, the time in the help.
        var showsTime = true
        let action: () -> Void

        var body: some View {
            let item = segment.item
            let style = CalendarColor.style(item)
            let inks = ChipColors.inks(style.hue)
            let r: CGFloat = 6
            let top: CGFloat = segment.continuesBefore ? 0 : r
            let bottom: CGFloat = segment.continuesAfter ? 0 : r
            let shape = UnevenRoundedRectangle(
                topLeadingRadius: top, bottomLeadingRadius: bottom,
                bottomTrailingRadius: bottom, topTrailingRadius: top, style: .continuous)
            let tight = height < 30
            let feathered = item.endEstimated && item.fill == .solid && !style.thin
            let roomForEnd = item.endEstimated && end != nil && height > 56
            Button(action: action) {
                ZStack(alignment: .topLeading) {
                    fill(shape: shape, style: style, inks: inks, feathered: feathered)
                    words(tight: tight, showsEnd: roomForEnd, style: style, inks: inks)
                        .padding(.horizontal, Space.s6)
                        .padding(.top, tight ? 1 : Space.scaled(3))
                    if roomForEnd, let end {
                        EndText(label: end)
                            .padding(.leading, Space.s6)
                            .frame(maxHeight: .infinity, alignment: .bottom)
                            .padding(.bottom, Space.s16)
                    }
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
                .clipShape(shape)
                // Selected: an even edge all the way round, in ink.
                .overlay(shape.strokeBorder(Palette.ink.opacity(selected ? 0.7 : 0), lineWidth: 1))
                .contentShape(shape)
            }
            .buttonStyle(.plain)
            .help(showsTime ? item.title : "\(item.title), \(time)")
        }

        @ViewBuilder
        private func fill(
            shape: UnevenRoundedRectangle, style: ChipStyle, inks: ChipInks, feathered: Bool
        ) -> some View {
            if feathered {
                // Known start, guessed body, no edge: the full fill for the
                // cap, the same hue thinner below, feathered out over the last
                // 20 points.
                let capStop = min(0.9, max(0, capHeight / max(1, height)))
                // Where the block runs on past midnight there is no fade:
                // the guess is still going.
                let fade = segment.continuesAfter ? 0 : 20 / max(1, height)
                shape
                    .fill(
                        LinearGradient(
                            stops: [
                                .init(color: inks.fill, location: capStop),
                                .init(color: inks.fill.opacity(0.5), location: min(1, capStop + 10 / max(1, height))),
                            ], startPoint: .top, endPoint: .bottom)
                    )
                    .mask(
                        LinearGradient(
                            stops: [
                                .init(color: .black, location: max(0, 1 - fade)),
                                .init(color: fade > 0 ? .clear : .black, location: 1),
                            ], startPoint: .top, endPoint: .bottom))
            } else {
                // The event pane's draft, on the week's hours: the same
                // dashed chip it is on the month.
                ChipFill(
                    style: style, inks: inks, shape: shape,
                    provisional: segment.item.id == EventForm.provisionalId)
            }
        }

        @ViewBuilder
        private func words(tight: Bool, showsEnd: Bool, style: ChipStyle, inks: ChipInks) -> some View {
            let item = segment.item
            let title = HStack(spacing: Space.s4) {
                // A commitment made in mail looks like a real event; the
                // envelope is what says where it came from.
                if item.isGhost, item.fill == .solid {
                    Image(systemName: "envelope")
                        .font(Typo.ui(8.5, weight: .medium))
                        .foregroundStyle(inks.ink)
                }
                Text(item.title)
                    .font(Typo.ui(11, weight: item.fill == .outline ? .medium : .semibold))
                    .foregroundStyle(Palette.ink)
                    .strikethrough(style.struck, color: Palette.inkFaint)
            }
            .lineLimit(1)
            // With the end said at the foot, the time line says only when it
            // starts.
            let meta = showsEnd ? time.components(separatedBy: " to ").first ?? time : time
            if !showsTime {
                title
            } else if tight {
                // A short block is one line. The time rides beside the title
                // only where both fit whole: a time cut to "8" beside a
                // title cut to "Earlier to..." says neither.
                ViewThatFits(in: .horizontal) {
                    HStack(alignment: .firstTextBaseline, spacing: Space.s6) {
                        title.fixedSize()
                        Text(meta)
                            .font(Typo.ui(10.5))
                            .foregroundStyle(inks.ink)
                            .lineLimit(1)
                            .fixedSize()
                    }
                    title
                }
            } else {
                VStack(alignment: .leading, spacing: 1) {
                    title
                    Text(meta)
                        .font(Typo.ui(10.5))
                        .foregroundStyle(inks.ink)
                        .lineLimit(1)
                }
            }
        }
    }
#endif
