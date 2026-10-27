// THE CALENDAR PAGE'S WEEK (wave 6, the founder: "lets add a week view
// too"): seven days side by side over one hours axis, and the all-day items
// and trips in a strip across the top.
//
// BUILT FROM THE MONTH'S PIECES, NOT BESIDE THEM. Each day is the month's own
// day column (`HoursWell`: the folded night, a line per hour, blocks side by
// side where they overlap, the now line, drag to create), under one
// `HoursGutter`. The strip is the month's trip bars (`SpanBar`) in the
// month's lanes, so a trip reads the same in both views. The bar above is the
// month's (`MonthBar`), turning weeks. Colour, the band language and the
// draft's dashed chip are all the month's, because they are the same views.
//
// WHAT IS MARKED. Today's date sits on the blue capsule, as in the month, and
// the now line runs in today's column only. The selected day's head takes the
// month's even 1pt edge in ink. Nothing else is tinted.
//
// THE DETAIL. A block clicked opens the agenda's own detail (`CalendarDetail`)
// beside the week on a wide window and in place of it on a narrow one, with
// a way back. With nothing open the week has the whole width: there is no day
// column to keep beside it, since the week is seven of them.
//
// WIDTH COMES FROM THE CONTAINER, both ways, as the month's does: every
// column is a seventh of what the GeometryReader was offered, less the
// gutter. Narrow columns shorten their words (`WeekGrid.dayLabel`,
// `showsBlockTime`); nothing scrolls sideways.
//
// All of the arithmetic is Lib/WeekGrid.swift and Lib/MonthGrid.swift.

#if os(macOS)
    import SwiftUI

    struct CalendarWeekView: View {
        @Environment(AppStore.self) private var store
        let model: WeekModel
        let place: MonthPlace
        /// The item whose detail is open, if any.
        let item: AgendaItem?
        /// The week and the detail side by side; otherwise one at a time.
        let twoPanes: Bool
        /// The event pane is open beside the page: the week alone.
        let drafting: Bool
        let paneWidth: CGFloat
        let now: Date
        let calendar: Calendar

        var body: some View {
            let title = WeekGrid.title(model.start, calendar: calendar)
            VStack(spacing: 0) {
                MonthBar(title: title, day: place.day, unit: .week)
                HStack(alignment: .top, spacing: Space.scaled(18)) {
                    if drafting || twoPanes || item == nil {
                        WeekGridView(
                            model: model, place: place, item: drafting ? nil : item, title: title, now: now,
                            calendar: calendar)
                    }
                    if !drafting, let item {
                        detail(item)
                            .frame(width: twoPanes ? paneWidth : nil)
                            .frame(maxWidth: twoPanes ? paneWidth : .infinity, maxHeight: .infinity, alignment: .top)
                    }
                }
                .padding(.horizontal, Space.page)
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
            }
        }

        private func detail(_ item: AgendaItem) -> some View {
            ScrollView(.vertical) {
                VStack(alignment: .leading, spacing: Space.s10) {
                    BackLink(text: Agenda.format(place.day, "EEE MMM d", calendar)) {
                        store.calendarMonthItem = nil
                    }
                    CalendarDetail(item: item, calendar: calendar)
                        // Keyed by the item: a different item is a different
                        // detail, its sources loading anew.
                        .id(item.id)
                }
                .padding(.top, Space.s4)
                .padding(.bottom, Space.scaled(28))
            }
            .scrollBounceBehavior(.basedOnSize)
            .scrollIndicators(.hidden)
        }
    }

    // MARK: - the grid

    private struct WeekGridView: View {
        @Environment(AppStore.self) private var store
        let model: WeekModel
        let place: MonthPlace
        let item: AgendaItem?
        let title: String
        let now: Date
        let calendar: Calendar

        /// Only the HEIGHT is measured off the scroll view (the month's
        /// rule: a vertical scroll view's width is not to be trusted).
        @State private var visibleHeight: CGFloat = 0

        private var nightHeight: CGFloat { Space.scaled(22) }

        var body: some View {
            GeometryReader { geo in
                let width = geo.size.width
                let gutter = HoursGutter.width + Space.s6
                let col = CGFloat(WeekGrid.columnWidth(width: Double(width), gutter: Double(gutter)))
                // The pane's draft is being placed: a click anywhere in a
                // day, on a block or beside it, is that day.
                let drafting = store.eventPaneOpen
                // The hours fill the height there is, between the month
                // column's own floor and ceiling; past the floor they scroll.
                let room = visibleHeight - nightHeight - Space.s6 - Space.scaled(20)
                let pph = min(Space.scaled(44), max(Space.scaled(20), floor(room / CGFloat(MonthGrid.dayHours))))
                VStack(alignment: .leading, spacing: 0) {
                    heads(col: col, gutter: gutter)
                    strip(col: col, gutter: gutter, width: width, drafting: drafting)
                    ScrollView(.vertical) {
                        hours(col: col, pph: pph, drafting: drafting)
                            .padding(.top, Space.s6)
                            .padding(.bottom, Space.scaled(20))
                            .frame(width: width, alignment: .topLeading)
                    }
                    .scrollBounceBehavior(.basedOnSize)
                    .scrollIndicators(.hidden)
                    .frame(width: width)
                    .frame(maxHeight: .infinity)
                    .onGeometryChange(for: CGFloat.self) { $0.size.height } action: { visibleHeight = $0 }
                    MonthFooter(
                        loading: !store.calendarLoaded, canCreate: store.calendarCanCreate,
                        drafting: drafting, week: true)
                }
                .frame(width: width, height: geo.size.height, alignment: .topLeading)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .accessibilityElement(children: .contain)
            .accessibilityLabel("Week of \(title)")
        }

        // MARK: the days' heads

        private func heads(col: CGFloat, gutter: CGFloat) -> some View {
            HStack(spacing: 0) {
                Color.clear.frame(width: gutter, height: 1)
                ForEach(model.columns, id: \.day) { column in
                    WeekDayHead(
                        label: WeekGrid.dayLabel(column.day, columnWidth: Double(col), calendar: calendar),
                        spoken: Agenda.format(column.day, "EEEE MMMM d", calendar),
                        selected: column.day == place.day,
                        today: calendar.isDate(column.day, inSameDayAs: now),
                        unlikely: column.unlikely.count,
                        showsUnlikely: Double(col) >= WeekGrid.unlikelyMinWidth,
                        width: col
                    ) { store.selectCalendarDay(column.day, open: false) }
                }
            }
            .padding(.bottom, Space.s2)
        }

        // MARK: the strip

        /// All-day items and trips across the days they cover, in the month's
        /// lanes. No strip at all in a week with none.
        @ViewBuilder
        private func strip(col: CGFloat, gutter: CGFloat, width: CGFloat, drafting: Bool) -> some View {
            let rows = WeekGrid.stripRows(model)
            if rows > 0 {
                ZStack(alignment: .topLeading) {
                    // Under the bars: a click beside one is its day.
                    HStack(spacing: 0) {
                        ForEach(model.columns, id: \.day) { column in
                            Color.clear
                                .frame(width: col)
                                .contentShape(Rectangle())
                                .onTapGesture { store.selectCalendarDay(column.day, open: false) }
                        }
                    }
                    .frame(maxHeight: .infinity)
                    .offset(x: gutter)
                    Text("all day")
                        .font(Typo.num(10))
                        .foregroundStyle(Palette.inkFaintest)
                        .lineLimit(1)
                        .frame(width: HoursGutter.width, height: MonthMetrics.rowHeight, alignment: .trailing)
                    ForEach(model.bars) { span in
                        SpanBar(
                            segment: span,
                            label: MonthGrid.spanLabel(span, calendar: calendar),
                            selected: item?.id == span.item.id,
                            cellWidth: col
                        ) {
                            // The bar's own first day in this week, as on the
                            // month: the selection lands where the eye is.
                            let first = model.columns[min(span.firstColumn, model.columns.count - 1)].day
                            store.selectCalendarItem(
                                span.item.id, on: max(first, calendar.startOfDay(for: span.item.start)))
                        }
                        .offset(
                            x: gutter + CGFloat(span.firstColumn) * col + MonthMetrics.inset,
                            y: CGFloat(span.lane) * MonthMetrics.step)
                        .allowsHitTesting(!drafting)
                    }
                    ForEach(Array(model.overflow.enumerated()), id: \.offset) { c, more in
                        if more > 0, c < model.columns.count {
                            let day = model.columns[c].day
                            Button {
                                // The month's day column lists every one.
                                store.setCalendarMode(.month)
                                store.selectCalendarDay(day, open: true)
                            } label: {
                                Text("+\(more) more")
                                    .font(Typo.ui(10, weight: .medium))
                                    .padding(.horizontal, Space.s4)
                                    .frame(height: MonthMetrics.rowHeight)
                            }
                            .buttonStyle(.textAction)
                            .pointingHand()
                            .help("Open \(Agenda.format(day, "EEE MMM d", calendar))")
                            .allowsHitTesting(!drafting)
                            .offset(
                                x: gutter + CGFloat(c) * col + MonthMetrics.inset,
                                y: CGFloat(model.lanes) * MonthMetrics.step)
                        }
                    }
                }
                .frame(
                    width: width, height: CGFloat(rows) * MonthMetrics.step - MonthMetrics.rowGap,
                    alignment: .topLeading)
                .padding(.bottom, Space.s4)
            }
        }

        // MARK: the hours

        private func hours(col: CGFloat, pph: CGFloat, drafting: Bool) -> some View {
            HStack(alignment: .top, spacing: Space.s6) {
                HoursGutter(pph: pph, nightHeight: nightHeight)
                ZStack(alignment: .topLeading) {
                    // One night cap for the week, under all seven wells.
                    RoundedRectangle(cornerRadius: 6, style: .continuous)
                        .fill(Palette.hairline.opacity(0.4))
                        .frame(width: col * 7, height: nightHeight - 2)
                    HStack(alignment: .top, spacing: 0) {
                        ForEach(model.columns, id: \.day) { column in
                            HoursWell(
                                column: column, pph: pph, nightHeight: nightHeight,
                                selected: item?.id,
                                now: calendar.isDate(column.day, inSameDayAs: now) ? now : nil,
                                calendar: calendar,
                                nightCap: false,
                                showsTime: WeekGrid.showsBlockTime(columnWidth: Double(col)),
                                tap: { store.selectCalendarDay(column.day, open: false) },
                                drafting: drafting
                            ) { picked in
                                store.selectCalendarItem(picked.id, on: column.day)
                            }
                            // A block never touches the day beside it.
                            .padding(.leading, 2)
                            .frame(width: col, alignment: .topLeading)
                            // The days ruled apart, from the first hour down.
                            .overlay(alignment: .leading) {
                                Rectangle()
                                    .fill(Palette.hairline)
                                    .frame(width: 0.5)
                                    .padding(.top, nightHeight)
                                    .allowsHitTesting(false)
                            }
                        }
                    }
                }
            }
        }
    }

    /// One day's head: "Tue 6", today's date on the blue capsule (the
    /// month's own mark), the selected day inside an even 1pt edge in ink.
    private struct WeekDayHead: View {
        let label: (weekday: String, number: String)
        let spoken: String
        let selected: Bool
        let today: Bool
        let unlikely: Int
        let showsUnlikely: Bool
        let width: CGFloat
        let action: () -> Void

        var body: some View {
            let shape = RoundedRectangle(cornerRadius: 8, style: .continuous)
            Button(action: action) {
                HStack(alignment: .center, spacing: Space.s2) {
                    Text(label.weekday)
                        .font(Typo.ui(10, weight: .medium))
                        .foregroundStyle(today ? Palette.inkDim : Palette.inkFaintest)
                        .lineLimit(1)
                        .fixedSize()
                    Text(label.number)
                        .font(Typo.num(11.5, weight: today ? .bold : .medium))
                        .foregroundStyle(today ? Color.white : Palette.inkDim)
                        .lineLimit(1)
                        .fixedSize()
                        .padding(.horizontal, Space.s4)
                        .padding(.vertical, 1.5)
                        .background(Capsule().fill(Palette.accent.opacity(today ? 1 : 0)))
                    Spacer(minLength: 0)
                    if unlikely > 0, showsUnlikely {
                        Text("\(unlikely) unlikely")
                            .font(Typo.ui(9.5, weight: .medium))
                            .foregroundStyle(Palette.inkFaintest)
                            .lineLimit(1)
                    }
                }
                .padding(.leading, Space.s6)
                .padding(.trailing, Space.s4)
                .frame(width: max(0, width), height: MonthMetrics.numberBand, alignment: .leading)
                .overlay(shape.strokeBorder(Palette.inkFaint.opacity(selected ? 0.75 : 0), lineWidth: 1).padding(1))
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .help(unlikely > 0 ? "\(spoken), \(unlikely) unlikely \(unlikely == 1 ? "plan" : "plans") from your mail" : spoken)
            .accessibilityLabel(today ? "\(spoken), today" : spoken)
            .accessibilityAddTraits(selected ? [.isButton, .isSelected] : .isButton)
        }
    }
#endif
