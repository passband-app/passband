// THE CALENDAR PAGE'S MONTH: a conventional month grid, and the selected
// day's column beside it.
//
// WAVE 3 (the founder: "the horizontal bar on each day is confusing. lets do
// top down like gmail does. lets add more color too."):
//   - Each cell lists its day top-down, one row per thing: trips and stays
//     first, as named bars across the cells they cover (flat where they run
//     on into the next row); then the day's own all-day items; then timed
//     items by start, "7:45pm  Dreamers Dinner". What does not fit folds into
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

    /// Month or agenda, in the page header, month first. Two options rather
    /// than the mock's Day / Week / Month: the day is the column beside the
    /// month and there is no week, so a control offering either would be a
    /// lie, and a disabled one would be furniture.
    struct CalendarModeSwitch: View {
        @Environment(AppStore.self) private var store

        var body: some View {
            GlassSegmented(
                options: CalendarViewMode.pickerOrder.map { (value: $0, label: $0 == .month ? "Month" : "Agenda") },
                selection: Binding(
                    get: { store.calendarMode },
                    set: { store.setCalendarMode($0) }))
        }
    }

    // MARK: - colours

    /// One hue's three tones as SwiftUI colours, each resolving per theme.
    struct ChipInks {
        /// A solid chip's body.
        let fill: Color
        /// The hue as text: a row's time, a calendar's name.
        let ink: Color
        /// The hue as a line: stripes, outlines, the agenda's mark.
        let edge: Color
    }

    /// `CalendarColor`'s tones, built once per hue: a month redraws on every
    /// poll tick, and the OKLab round trip is cheap but not free.
    @MainActor
    enum ChipColors {
        private static var cache: [ChipHue: ChipInks] = [:]

        static func inks(_ hue: ChipHue) -> ChipInks {
            if let hit = cache[hue] { return hit }
            let light = CalendarColor.tones(for: hue, dark: false)
            let dark = CalendarColor.tones(for: hue, dark: true)
            let out = ChipInks(
                fill: Color(light: light.fill.hex, dark: dark.fill.hex),
                ink: Color(light: light.ink.hex, dark: dark.ink.hex),
                edge: Color(light: light.edge.hex, dark: dark.edge.hex))
            // A user has a handful of calendars; a bound keeps a daemon that
            // sends a new colour on every event from growing this forever.
            if cache.count > 256 { cache.removeAll() }
            cache[hue] = out
            return out
        }

        static func inks(_ item: AgendaItem) -> ChipInks { inks(CalendarColor.style(item).hue) }
    }

    /// A chip's body on the likelihood scale, in its hue. Solid is the fill;
    /// striped is a thinner fill, hatched, with an even edge; an outline is the
    /// edge alone, an EMPTY container (the words on it never fade). Thin (a
    /// cancelled row, an unlikely one out of its fold) is a fainter outline.
    struct ChipFill<S: InsettableShape>: View {
        let style: ChipStyle
        let inks: ChipInks
        let shape: S
        var stripeSpacing: CGFloat = 6

        var body: some View {
            ZStack {
                switch style.thin ? .outline : style.fill {
                case .solid:
                    shape.fill(inks.fill)
                case .striped:
                    shape.fill(inks.fill.opacity(0.45))
                    Stripes(spacing: stripeSpacing)
                        .stroke(inks.edge.opacity(0.34), lineWidth: 1.5)
                        .clipShape(shape)
                    shape.strokeBorder(inks.edge.opacity(0.85), lineWidth: 1)
                case .outline:
                    shape.strokeBorder(inks.edge.opacity(style.thin ? 0.45 : 0.9), lineWidth: 1)
                }
            }
        }
    }

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
        let paneWidth: CGFloat
        let unlikelyOpen: Bool
        let toggleUnlikely: () -> Void
        let now: Date
        let calendar: Calendar

        var body: some View {
            VStack(spacing: 0) {
                MonthBar(title: MonthGrid.title(place.month, calendar: calendar), day: place.day)
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

    /// The month's own bar: which month, the way to the ones beside it, back
    /// to today, and a new event on the selected day. The page header above
    /// it carries the mode switch.
    private struct MonthBar: View {
        @Environment(AppStore.self) private var store
        let title: String
        let day: Date

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
                // Only where the daemon lists a calendar the user can write
                // to: an older one never shows the control at all.
                if store.calendarCanCreate {
                    ChromeChip(text: "New event", icon: "plus", tone: Palette.ink, help: "New event ( n )") {
                        store.openEventEditor(day: day)
                    }
                }
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

        /// The rows' visible height. Only the HEIGHT is measured off the
        /// scroll view: it is offered a height and takes it, whatever its
        /// content. Its width is not trustworthy (see the file's header).
        @State private var visibleHeight: CGFloat = 0

        var body: some View {
            GeometryReader { geo in
                let width = geo.size.width
                let cellWidth = CGFloat(MonthGrid.cellWidth(gridWidth: Double(width)))
                let rows = max(1, model.weeks.count)
                let busiest = model.weeks.map(\.lanes).max() ?? 0
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
                    }
                    .scrollBounceBehavior(.basedOnSize)
                    .scrollIndicators(.hidden)
                    .frame(width: width)
                    .frame(maxHeight: .infinity)
                    .onGeometryChange(for: CGFloat.self) { $0.size.height } action: { visibleHeight = $0 }
                    MonthFooter(loading: !store.calendarLoaded, canCreate: store.calendarCanCreate)
                }
                .frame(width: width, height: geo.size.height, alignment: .topLeading)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .accessibilityElement(children: .contain)
            .accessibilityLabel(MonthGrid.title(model.month, calendar: calendar))
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
    private struct MonthFooter: View {
        let loading: Bool
        let canCreate: Bool

        var body: some View {
            HStack(spacing: Space.s12) {
                if loading {
                    Text("loading your calendar…")
                        .font(Typo.micro)
                        .foregroundStyle(Palette.inkFaintest)
                        .lineLimit(1)
                }
                Spacer(minLength: Space.s8)
                if canCreate {
                    KeyHint("n", "new event")
                }
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
        let month: Date
        let place: MonthPlace
        let item: AgendaItem?
        let twoPanes: Bool
        let cellWidth: CGFloat
        let height: CGFloat
        let now: Date
        let calendar: Calendar

        var body: some View {
            let slots = MonthGrid.slots(
                cellHeight: Double(height), lanes: week.lanes,
                numberBand: Double(MonthMetrics.numberBand), rowHeight: Double(MonthMetrics.rowHeight),
                rowGap: Double(MonthMetrics.rowGap), foot: Double(MonthMetrics.foot))
            ZStack(alignment: .topLeading) {
                HStack(spacing: 0) {
                    ForEach(Array(week.days.enumerated()), id: \.element.id) { i, day in
                        MonthCell(
                            day: day,
                            label: MonthGrid.cellLabel(day.day, month: month, calendar: calendar),
                            selected: day.day == place.day,
                            today: calendar.isDate(day.day, inSameDayAs: now),
                            fold: MonthGrid.fold(
                                items: day.items.count,
                                hiddenSpans: i < week.overflow.count ? week.overflow[i] : 0,
                                slots: slots),
                            lanes: week.lanes,
                            selectedItem: item?.id,
                            width: cellWidth,
                            now: now,
                            calendar: calendar,
                            open: { store.selectCalendarDay(day.day, open: !twoPanes) },
                            pick: { picked in store.selectCalendarItem(picked.id, on: day.day) })
                    }
                }
                ForEach(week.spans) { span in
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
        let fold: CellFold
        let lanes: Int
        let selectedItem: String?
        let width: CGFloat
        let now: Date
        let calendar: Calendar
        let open: () -> Void
        let pick: (AgendaItem) -> Void

        var body: some View {
            let shape = RoundedRectangle(cornerRadius: 8, style: .continuous)
            let shown = Array(day.items.prefix(fold.shown))
            let nowAt = today ? MonthGrid.nowSlot(day.items, now: now) : nil
            VStack(alignment: .leading, spacing: MonthMetrics.rowGap) {
                numberRow
                    .frame(height: MonthMetrics.numberBand - MonthMetrics.rowGap, alignment: .center)
                    // The trips' lanes: room kept, drawn by the row above.
                    .padding(.bottom, CGFloat(lanes) * MonthMetrics.step)
                ForEach(Array(shown.enumerated()), id: \.element.id) { i, item in
                    CellRow(
                        item: item,
                        time: MonthGrid.cellTime(
                            item, cellWidth: Double(width), minWidth: MonthMetrics.timeMinWidth,
                            calendar: calendar),
                        selected: selectedItem == item.id,
                        width: width - 2 * MonthMetrics.inset
                    ) { pick(item) }
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
                }
                if fold.more > 0 {
                    Button(action: open) {
                        Text("+\(fold.more) more")
                            .font(Typo.ui(10, weight: .medium))
                            .padding(.horizontal, Space.s4)
                            .frame(height: MonthMetrics.rowHeight)
                    }
                    .buttonStyle(.textAction)
                    .pointingHand()
                    .help("Open \(Agenda.format(day.day, "EEE MMM d", calendar))")
                }
            }
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
                        .font(Typo.ui(10.5, weight: item.fill == .outline || style.thin ? .regular : .medium))
                        .foregroundStyle(Palette.ink)
                        .strikethrough(style.struck, color: Palette.inkFaint)
                        .lineLimit(1)
                        .truncationMode(.tail)
                }
                .padding(.horizontal, Space.s4)
                .frame(width: max(0, width), height: MonthMetrics.rowHeight, alignment: .leading)
                .background { ChipFill(style: style, inks: inks, shape: shape, stripeSpacing: 5) }
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
    private struct SpanBar: View {
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
                .background { ChipFill(style: style, inks: inks, shape: shape, stripeSpacing: 5) }
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
                // The well's width comes from the pane, never from its blocks.
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
            let style = CalendarColor.style(s.item)
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
                    .foregroundStyle(Palette.ink)
                    .lineLimit(1)
                    .padding(.horizontal, Space.s6)
                    .frame(width: w, height: height, alignment: .leading)
                    .background { ChipFill(style: style, inks: ChipColors.inks(style.hue), shape: shape) }
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
                            .padding(.bottom, Space.scaled(16))
                    }
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
                .clipShape(shape)
                // Selected: an even edge all the way round, in ink.
                .overlay(shape.strokeBorder(Palette.ink.opacity(selected ? 0.7 : 0), lineWidth: 1))
                .contentShape(shape)
            }
            .buttonStyle(.plain)
            .help(item.title)
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
                ChipFill(style: style, inks: inks, shape: shape)
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
            if tight {
                HStack(alignment: .firstTextBaseline, spacing: Space.s6) {
                    title.layoutPriority(1)
                    Text(meta)
                        .font(Typo.ui(10.5))
                        .foregroundStyle(inks.ink)
                        .lineLimit(1)
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
