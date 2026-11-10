// THE CALENDAR PAGE: the month (Views/CalendarMonth.swift), which it opens
// on, the week (Views/CalendarWeek.swift) and an agenda, switched with `m` /
// `w` / `a` and the header's control. From
// any, `n` makes an event on one of the user's own Google calendars and
// `e` changes one, in a pane that takes half the window the way the composer
// does (Views/CalendarEventPane.swift). While that pane is open this page has
// the other half: the month keeps its grid and puts the day column and the
// detail away, the agenda keeps its list, and both stay live.
//
// The agenda: today and the days after it, real events
// and the ghosts Passband found in mail in one time order, with the next
// commitment called out and today's shape as one strip. Most days this page
// is the whole calendar, so it reads top to bottom like a day planner rather
// than across like a table: a measure-wide list, and the selected row's detail
// beside it (below it, in place, when the window is too narrow for two
// columns).
//
// The visual language is docs/calendar-ux-preview.html, and it is binding:
//   - Fill is the likelihood scale. Solid is a real event or a commitment,
//     striped is likely, an outline is maybe, unlikely folds into a count.
//   - A ghost is an EMPTY container, never a faded one. Its words are at full
//     strength, so it never reads as a disabled control.
//   - A guessed end is softer and says "ends ~11pm".
//   - A real event wears its calendar's colour and a ghost the accent's,
//     both brought inside the palette by Lib/CalendarColor.swift. Blue as a
//     STATE is now, next and today; selection is an even edge.
//   - Nothing on this page offers to write mail.
//
// All of the arithmetic (days, order, fills, folds, every time string) is
// Lib/Agenda.swift, pure and tested; this file only draws it.

#if os(macOS)
    import SwiftUI

    struct CalendarPage: View {
        @Environment(AppStore.self) private var store

        /// The row whose detail is showing. Nil follows the default (the next
        /// commitment, else the first row), so the detail is never empty
        /// while there is anything to show, and a dismissed row's selection
        /// falls back rather than pointing at nothing.
        @State private var selection: String?
        /// Set only by j/k: the list follows the keyboard, never a click (a
        /// row that recentres under the pointer that just clicked it reads as
        /// the page lurching).
        @State private var scrollTarget: String?
        /// Folds the user has opened, by day: "u" + day for unlikely, "e" +
        /// day for earlier today.
        @State private var openFolds: Set<String> = []
        @State private var pageWidth: CGFloat = .infinity
        /// The last agenda built, so a body re-run that changed nothing it
        /// is built from (a window resize, a selection) does not rebuild it.
        @State private var memo = AgendaMemo()
        /// The same for the month: its grid and the selected day's column.
        @State private var monthMemo = MonthMemo()

        /// Two columns once the list's measure and the detail both fit. Never
        /// while the event pane is open: the detail is away until it closes.
        private var twoColumns: Bool {
            !store.eventPaneOpen
                && pageWidth >= Self.listMeasure + Self.detailWidth + Self.columnGap + Space.scaled(44)
        }
        private static var listMeasure: CGFloat { Typo.measure(560) }
        private static var detailWidth: CGFloat { Typo.measure(340) }
        /// The gutter between the agenda and its detail: the board's column
        /// gap, so every two-column page splits the same way.
        private static var columnGap: CGFloat { Space.s16 }

        /// The month's grid and its pane side by side: seven cells of about
        /// 78 points, and the column at the detail's own width (the pane
        /// shows one or the other).
        private var monthTwoPanes: Bool {
            // The month pads by the page gutter on both sides
            // (`CalendarMonth`), so that is what it reserves.
            pageWidth >= Self.monthGridMin + Self.detailWidth + Space.scaled(18) + 2 * Space.page
        }
        private static var monthGridMin: CGFloat { Typo.measure(546) }

        // THIS BODY NEVER READS `stats`. The poller reassigns it nearly every
        // ten seconds, and a reader here would rebuild the whole agenda on
        // each tick; everything about connecting and the sync's health is in
        // `CalendarStatusLines`, a leaf that takes those ticks alone.
        var body: some View {
            // A minute clock: "Next, in 50 min", the earlier fold and the
            // strip's now tick all move with it. The feed itself changes on
            // the poll's cadence (the keeper below).
            TimelineView(.everyMinute) { context in
                let now = context.date
                let cal = Calendar.current
                // Two subtrees on purpose, each with its own key set: the
                // agenda's state lives in this view's @State and the month's
                // in the store, so neither is lost by the switch.
                switch store.calendarMode {
                case .agenda:
                    let days = memo.days(store.calendarFeed, now: now, calendar: cal)
                    let next = Agenda.next(days, now: now)
                    let rows = navigable(days)
                    let selected = rows.first { $0.id == selection } ?? defaultSelection(next, rows)
                    page(days: days, next: next, rows: rows, selected: selected, now: now, cal: cal)
                case .month:
                    monthPage(now: now, cal: cal)
                case .week:
                    weekPage(now: now, cal: cal)
                }
            }
            .onGeometryChange(for: CGFloat.self) { $0.size.width } action: { pageWidth = $0 }
            .background { CalendarRefreshKeeper() }
            .onAppear {
                Analytics.capture(
                    "calendar_opened",
                    ["granted": store.calendarGranted, "view": store.calendarMode.rawValue])
            }
            // The month on screen is part of what is asked for, so turning
            // the page (or switching mode) asks at once rather than at the
            // next poll tick. Only on a change: the keeper's first run covers
            // the page appearing.
            .onChange(of: store.calendarRequestMonth(now: Date())) {
                Task { await store.refreshCalendar() }
            }
            // A day opened in place of the grid means nothing once the grid
            // and the column fit side by side again.
            .onChange(of: monthTwoPanes) { _, two in
                if two, store.calendarDayOpen { store.calendarDayOpen = false }
            }
        }

        // MARK: - the month

        private func monthPage(now: Date, cal: Calendar) -> some View {
            let place = store.calendarMonthPlace(now: now, calendar: cal)
            let drafting = store.eventPaneOpen
            // The pane's draft, as a row of its own on the grid: where its
            // fields would put it, standing in for the event being edited.
            let model = monthMemo.month(
                store.calendarFeed, month: place.month, calendar: cal,
                provisional: store.calendarProvisional(now: now, calendar: cal),
                editing: drafting ? store.calendarEditor?.event : nil)
            let column = monthMemo.column(day: place.day, calendar: cal)
            let unlikelyKey = "u\(place.day.timeIntervalSince1970)"
            let unlikelyOpen = openFolds.contains(unlikelyKey)
            let order = column.order + (unlikelyOpen ? column.unlikely : [])
            let item = store.calendarMonthItem.flatMap { id in monthMemo.items.first { $0.id == id } }
            return VStack(spacing: 0) {
                CalendarStatusLines()
                CalendarMonthView(
                    model: model, column: column, place: place, item: item,
                    twoPanes: monthTwoPanes, drafting: drafting, paneWidth: Self.detailWidth,
                    unlikelyOpen: unlikelyOpen, toggleUnlikely: { toggle(unlikelyKey) },
                    now: now, calendar: cal)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
            .keyBindings(.modal, monthBindings(order: order, item: item, day: place.day))
        }

        // MARK: - the week

        /// The week the month's selected day is in: the same place, the same
        /// selected item, the same draft on the grid.
        private func weekPage(now: Date, cal: Calendar) -> some View {
            let place = store.calendarMonthPlace(now: now, calendar: cal)
            let drafting = store.eventPaneOpen
            let model = monthMemo.week(
                store.calendarFeed, day: place.day, calendar: cal,
                provisional: store.calendarProvisional(now: now, calendar: cal),
                editing: drafting ? store.calendarEditor?.event : nil)
            let order = model.columns.first { $0.day == place.day }?.order ?? []
            let item = store.calendarMonthItem.flatMap { id in monthMemo.items.first { $0.id == id } }
            return VStack(spacing: 0) {
                CalendarStatusLines()
                CalendarWeekView(
                    model: model, place: place, item: item, twoPanes: monthTwoPanes, drafting: drafting,
                    paneWidth: Self.detailWidth, now: now, calendar: cal)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
            .keyBindings(.modal, weekBindings(order: order, item: item, day: place.day))
        }

        @ViewBuilder
        private func page(
            days: [AgendaDay], next: (item: AgendaItem, ongoing: Bool)?, rows: [AgendaItem],
            selected: AgendaItem?, now: Date, cal: Calendar
        ) -> some View {
            VStack(spacing: 0) {
                CalendarStatusLines()
                if store.calendarFeed == nil && !store.calendarLoaded {
                    BandNote("loading your calendar…")
                        .padding(.horizontal, DeskRow.listInset)
                    Spacer(minLength: 0)
                } else {
                    ScrollViewReader { proxy in
                        HStack(alignment: .top, spacing: Self.columnGap) {
                            ScrollView(.vertical) {
                                agenda(
                                    days: days, next: next, selected: selected, now: now, cal: cal
                                )
                                .frame(maxWidth: twoColumns ? Self.listMeasure : .infinity, alignment: .leading)
                                .padding(.top, Space.s14)
                                .padding(.bottom, Space.scaled(28))
                            }
                            .scrollIndicators(.hidden)
                            .scrollShadowRoom(topFade: Space.s14)
                            .frame(maxWidth: twoColumns ? Self.listMeasure : .infinity, alignment: .top)

                            if twoColumns {
                                ScrollView(.vertical) {
                                    detailColumn(selected, cal: cal)
                                        .padding(.top, Space.s14)
                                        .padding(.bottom, Space.scaled(28))
                                }
                                .scrollBounceBehavior(.basedOnSize)
                                .scrollIndicators(.hidden)
                                .scrollShadowRoom(topFade: Space.s14)
                                .frame(width: Self.detailWidth)
                            }
                            Spacer(minLength: 0)
                        }
                        // The agenda's rows bleed: their fill reaches into the
                        // leading inset and their words land on the page
                        // gutter (`DeskRow`). The trailing edge is the page's.
                        .padding(.leading, DeskRow.listInset)
                        .padding(.trailing, Space.page)
                        .onChange(of: scrollTarget) { _, id in
                            guard let id else { return }
                            withAnimation(Motion.scrollFollow) { proxy.scrollTo(id, anchor: .center) }
                        }
                    }
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
            .keyBindings(.modal, bindings(rows: rows, selected: selected))
        }

        // MARK: - the agenda column

        @ViewBuilder
        private func agenda(
            days: [AgendaDay], next: (item: AgendaItem, ongoing: Bool)?, selected: AgendaItem?,
            now: Date, cal: Calendar
        ) -> some View {
            let today = days.first
            VStack(alignment: .leading, spacing: Space.s16) {
                if let next {
                    NextCard(
                        item: next.item,
                        lead: Agenda.nextLead(next.item, ongoing: next.ongoing, now: now, calendar: cal),
                        place: next.item.location,
                        span: spanText(next.item, cal: cal),
                        selected: selected?.id == next.item.id
                    ) { select(next.item) }
                }
                if let today, today.visible.contains(where: { !$0.allDay }) || !today.earlier.isEmpty {
                    TodayStrip(day: today, now: now, calendar: cal)
                }
                ForEach(days) { day in
                    daySection(day, selected: selected, now: now, cal: cal)
                }
                if days.allSatisfy(\.isEmpty) {
                    Text("Nothing else coming up. Tickets, bookings and invitations Passband finds in your mail will show here.")
                        .font(Typo.rowSub)
                        .foregroundStyle(Palette.inkFaintest)
                        .fixedSize(horizontal: false, vertical: true)
                        .padding(.horizontal, DeskRow.hPadding)
                }
                // The daemon cut the list short: said where the list ends,
                // in the faintest ink, because nothing is wrong.
                if let note = CalendarGate.truncatedNote(store.calendarFeed?.calendar) {
                    Text(note)
                        .font(Typo.micro)
                        .foregroundStyle(Palette.inkFaintest)
                        .padding(.horizontal, DeskRow.hPadding)
                }
            }
        }

        private func daySection(
            _ day: AgendaDay, selected: AgendaItem?, now: Date, cal: Calendar
        ) -> some View {
            let title = Agenda.dayTitle(day.day, now: now, calendar: cal)
            let isToday = cal.isDate(day.day, inSameDayAs: now)
            let earlierKey = "e\(day.day.timeIntervalSince1970)"
            let unlikelyKey = "u\(day.day.timeIntervalSince1970)"
            return VStack(alignment: .leading, spacing: 1) {
                HStack(alignment: .firstTextBaseline) {
                    Text(title.title)
                        .font(Typo.ui(12, weight: .semibold))
                        .foregroundStyle(isToday ? Palette.ink : Palette.inkDim)
                    Spacer(minLength: Space.s8)
                    Text(title.detail)
                        .font(Typo.num(11))
                        .foregroundStyle(Palette.inkFaintest)
                }
                .padding(.horizontal, DeskRow.hPadding)
                .padding(.bottom, Space.s4)

                if !day.earlier.isEmpty {
                    FoldLine(
                        text: "\(day.earlier.count) earlier today",
                        open: openFolds.contains(earlierKey)
                    ) { toggle(earlierKey) }
                    if openFolds.contains(earlierKey) {
                        rows(day.earlier, day: day, selected: selected, cal: cal, thin: false)
                    }
                }
                if day.visible.isEmpty && day.earlier.isEmpty && day.unlikely.isEmpty {
                    Text(isToday ? "Nothing on today." : "Nothing on.")
                        .font(Typo.rowSub)
                        .foregroundStyle(Palette.inkFaintest)
                        .padding(.horizontal, DeskRow.hPadding)
                        .padding(.vertical, Space.s4)
                }
                rows(day.visible, day: day, selected: selected, cal: cal, thin: false)
                if !day.unlikely.isEmpty {
                    FoldLine(
                        text: "\(day.unlikely.count) unlikely",
                        open: openFolds.contains(unlikelyKey)
                    ) { toggle(unlikelyKey) }
                    if openFolds.contains(unlikelyKey) {
                        rows(day.unlikely, day: day, selected: selected, cal: cal, thin: true)
                    }
                }
            }
        }

        @ViewBuilder
        private func rows(
            _ items: [AgendaItem], day: AgendaDay, selected: AgendaItem?, cal: Calendar, thin: Bool
        ) -> some View {
            ForEach(items) { item in
                let isSelected = selected?.id == item.id
                VStack(alignment: .leading, spacing: Space.s6) {
                    AgendaRow(
                        item: item,
                        time: Agenda.timeLabel(item, on: day.day, calendar: cal),
                        end: Agenda.endLabel(item, on: day.day, calendar: cal),
                        thin: thin,
                        selected: isSelected
                    ) { select(item) }
                    // NARROW WINDOWS: the detail opens in place, under its
                    // row, rather than in a column there is no room for. Not
                    // beside the event pane, where the list is all there is.
                    if !twoColumns, isSelected, !store.eventPaneOpen {
                        CalendarDetail(item: item, calendar: cal)
                            .padding(.leading, DeskRow.hPadding)
                            .padding(.bottom, Space.s8)
                            .transition(.opacity)
                    }
                }
                .id(item.id)
            }
        }

        // MARK: - the detail column

        @ViewBuilder
        private func detailColumn(_ selected: AgendaItem?, cal: Calendar) -> some View {
            if let selected {
                CalendarDetail(item: selected, calendar: cal)
                    // Keyed by the row: a different row is a different
                    // detail, with its own sources loading, not this one's
                    // state carried over.
                    .id(selected.id)
            }
        }

        // MARK: - keys

        // NO KEY HINT BAR. The verbs are taught where they act, on the
        // detail (each one a clickable key hint), and a bar under the list
        // repeating them read as the same three keys printed twice.

        /// The page's keys. They decline while ANY modal overlay is up (the
        /// connect card, the shortcuts help, What's New, the reconnect
        /// prompt): those share this key context, and one that does not bind
        /// x itself would otherwise let x dismiss a ghost behind it. The
        /// overlay's own keys are untouched; declining hands them on.
        private func bindings(rows: [AgendaItem], selected: AgendaItem?) -> [KeyBinding] {
            // The verbs on the selected row also decline beside the event
            // pane: its detail is away, and a ghost must not be dismissed by
            // a letter meant for a field the caret had just left.
            let free = { !store.modalOverlayOpen && !store.eventPaneOpen }
            func step(_ delta: Int) -> Bool {
                guard !store.modalOverlayOpen, !rows.isEmpty else { return false }
                let at = selected.flatMap { s in rows.firstIndex { $0.id == s.id } } ?? -1
                let i = max(0, min(rows.count - 1, at + delta))
                selection = rows[i].id
                scrollTarget = rows[i].id
                return true
            }
            return modeBindings() + writeBindings(selected: selected, day: nil) + changeBindings(selected: selected) + [
                KeyBinding(declining: "j", "next") { step(+1) },
                KeyBinding(declining: "k", "previous") { step(-1) },
                KeyBinding(declining: "ArrowDown", "next") { step(+1) },
                KeyBinding(declining: "ArrowUp", "previous") { step(-1) },
                KeyBinding(declining: "Enter", "open the mail behind it") {
                    guard free(), let selected else { return false }
                    return open(selected)
                },
                KeyBinding(declining: "y", "going") {
                    guard free() else { return false }
                    // A real invitation: yes, through Google.
                    guard let ghost = selected?.ghost else { return store.replyByKey(selected?.event, .accepted) }
                    let state: GhostState? = ghost.state == .going ? nil : .going
                    Task { await store.markGhost(ghost, state, viaKey: true) }
                    return true
                },
                KeyBinding(declining: "x", "dismiss") {
                    guard free() else { return false }
                    // A real invitation: no, through Google.
                    guard let ghost = selected?.ghost else { return store.replyByKey(selected?.event, .declined) }
                    Task { await store.markGhost(ghost, .dismissed, viaKey: true) }
                    return true
                },
            ]
        }

        /// `n`, `e` and ⌫, on both: a new event (on the month's selected day,
        /// or today from the agenda), and the selected event edited or
        /// deleted. Each declines where it does not apply: a daemon that lists
        /// no writable calendar, a ghost, an event that is not the user's.
        private func writeBindings(selected: AgendaItem?, day: Date?) -> [KeyBinding] {
            let free = { !store.modalOverlayOpen && !store.eventPaneOpen }
            return [
                KeyBinding(declining: "n", "new event") {
                    guard free(), store.calendarCanCreate else { return false }
                    store.openEventEditor(day: day)
                    return true
                },
                KeyBinding(declining: "e", "edit event") {
                    guard free(), let event = selected?.event, CalendarWrite.canEdit(event) else { return false }
                    store.openEventEditor(editing: event)
                    return true
                },
                KeyBinding(declining: "Backspace", "delete event") {
                    guard free(), let event = selected?.event, CalendarWrite.canEdit(event) else { return false }
                    store.askDeleteEvent(event)
                    return true
                },
            ]
        }

        /// ⇧A and ⇧K, on all three: accept, or keep mine, a change another
        /// sender's mail offers on the selected card (`LinkedChange`). Exact
        /// case, and declining where nothing is offered, so a bare `a` is
        /// still the agenda and `k` still steps back.
        private func changeBindings(selected: AgendaItem?) -> [KeyBinding] {
            let free = { !store.modalOverlayOpen && !store.eventPaneOpen }
            return [
                KeyBinding(declining: "A", "accept the change", exactCase: true) {
                    guard free(), let change = selected?.pendingChange else { return false }
                    Task { await store.answerLinkedChange(change, accept: true, viaKey: true) }
                    return true
                },
                KeyBinding(declining: "K", "keep mine", exactCase: true) {
                    guard free(), let change = selected?.pendingChange else { return false }
                    Task { await store.answerLinkedChange(change, accept: false, viaKey: true) }
                    return true
                },
            ]
        }

        /// `m`, `w` and `a`, on all three: the month, the week, the agenda.
        private func modeBindings() -> [KeyBinding] {
            [
                KeyBinding(declining: "w", "week") {
                    guard !store.modalOverlayOpen else { return false }
                    store.setCalendarMode(.week)
                    return true
                },
                KeyBinding(declining: "a", "agenda") {
                    guard !store.modalOverlayOpen else { return false }
                    store.setCalendarMode(.agenda)
                    return true
                },
                KeyBinding(declining: "m", "month") {
                    guard !store.modalOverlayOpen else { return false }
                    store.setCalendarMode(.month)
                    return true
                },
            ]
        }

        /// The month's keys. Arrows move the day (and turn the page off the
        /// grid's edge), `t` is today, `[` `]` and `,` `.` the months; j/k
        /// step through the selected day's items. Enter on a selected item
        /// opens its mail; on the day itself (nothing selected) it makes a
        /// new event there, or, with no calendar to write to, goes one level
        /// in: the day (where it is not already beside the grid), then its
        /// first item.
        /// Escape is the routed host's (`AppStore.calendarMonthBack`).
        private func monthBindings(order: [AgendaItem], item: AgendaItem?, day: Date) -> [KeyBinding] {
            // Beside the event pane the grid still MOVES (arrows, `t`, the
            // months), but nothing steps through or acts on the day's items:
            // the column and the detail they live in are away.
            let free = { !store.modalOverlayOpen && !store.eventPaneOpen }
            func move(_ m: MonthMove) -> Bool {
                guard !store.modalOverlayOpen else { return false }
                store.moveCalendarMonth(m)
                return true
            }
            func step(_ delta: Int) -> Bool {
                guard free(), !order.isEmpty else { return false }
                let at = item.flatMap { i in order.firstIndex { $0.id == i.id } }
                let i = at.map { max(0, min(order.count - 1, $0 + delta)) } ?? (delta > 0 ? 0 : order.count - 1)
                store.calendarMonthItem = order[i].id
                return true
            }
            return modeBindings() + writeBindings(selected: item, day: day) + changeBindings(selected: item) + [
                KeyBinding(declining: "ArrowLeft", "previous day") { move(.days(-1)) },
                KeyBinding(declining: "ArrowRight", "next day") { move(.days(1)) },
                KeyBinding(declining: "ArrowUp", "a week back") { move(.days(-7)) },
                KeyBinding(declining: "ArrowDown", "a week on") { move(.days(7)) },
                KeyBinding(declining: "t", "today") { move(.today) },
                KeyBinding(declining: "[", "previous month") { move(.months(-1)) },
                KeyBinding(declining: "]", "next month") { move(.months(1)) },
                KeyBinding(declining: ",", "previous month") { move(.months(-1)) },
                KeyBinding(declining: ".", "next month") { move(.months(1)) },
                KeyBinding(declining: "j", "next") { step(+1) },
                KeyBinding(declining: "k", "previous") { step(-1) },
                KeyBinding(declining: "Enter", "open") {
                    guard free() else { return false }
                    if let item { return open(item) }
                    // A day with nothing selected on it: a new event there.
                    if store.calendarCanCreate {
                        store.openEventEditor(day: day)
                        return true
                    }
                    if !monthTwoPanes, !store.calendarDayOpen {
                        store.calendarDayOpen = true
                        return true
                    }
                    guard let first = order.first else { return false }
                    store.calendarMonthItem = first.id
                    return true
                },
                KeyBinding(declining: "y", "going") {
                    guard free() else { return false }
                    // A real invitation: yes, through Google.
                    guard let ghost = item?.ghost else { return store.replyByKey(item?.event, .accepted) }
                    let state: GhostState? = ghost.state == .going ? nil : .going
                    Task { await store.markGhost(ghost, state, viaKey: true) }
                    return true
                },
                KeyBinding(declining: "x", "dismiss") {
                    guard free() else { return false }
                    // A real invitation: no, through Google.
                    guard let ghost = item?.ghost else { return store.replyByKey(item?.event, .declined) }
                    Task { await store.markGhost(ghost, .dismissed, viaKey: true) }
                    return true
                },
            ]
        }

        /// The week's keys. Left and right move the day (and turn the week off
        /// its edge), `[` `]` and `,` `.` turn a week, `t` is today. j / k and
        /// the up and down arrows walk the selected day's items, top-down as
        /// drawn. Enter on an item opens its mail; on the day, a new event
        /// there (else the day's first item). The
        /// rest (`n`, `e`, ⌫, `y`, `x`) are the month's.
        private func weekBindings(order: [AgendaItem], item: AgendaItem?, day: Date) -> [KeyBinding] {
            let free = { !store.modalOverlayOpen && !store.eventPaneOpen }
            func move(_ m: MonthMove) -> Bool {
                guard !store.modalOverlayOpen else { return false }
                store.moveCalendarMonth(m)
                return true
            }
            func step(_ delta: Int) -> Bool {
                guard free(), let id = WeekGrid.step(order, from: item?.id, by: delta) else { return false }
                store.calendarMonthItem = id
                return true
            }
            return modeBindings() + writeBindings(selected: item, day: day) + changeBindings(selected: item) + [
                KeyBinding(declining: "ArrowLeft", "previous day") { move(.days(-1)) },
                KeyBinding(declining: "ArrowRight", "next day") { move(.days(1)) },
                KeyBinding(declining: "ArrowUp", "previous") { step(-1) },
                KeyBinding(declining: "ArrowDown", "next") { step(+1) },
                KeyBinding(declining: "t", "today") { move(.today) },
                KeyBinding(declining: "[", "previous week") { move(.days(-7)) },
                KeyBinding(declining: "]", "next week") { move(.days(7)) },
                KeyBinding(declining: ",", "previous week") { move(.days(-7)) },
                KeyBinding(declining: ".", "next week") { move(.days(7)) },
                KeyBinding(declining: "j", "next") { step(+1) },
                KeyBinding(declining: "k", "previous") { step(-1) },
                KeyBinding(declining: "Enter", "open") {
                    guard free() else { return false }
                    if let item { return open(item) }
                    if store.calendarCanCreate {
                        store.openEventEditor(day: day)
                        return true
                    }
                    guard let first = order.first else { return false }
                    store.calendarMonthItem = first.id
                    return true
                },
                KeyBinding(declining: "y", "going") {
                    guard free() else { return false }
                    // A real invitation: yes, through Google.
                    guard let ghost = item?.ghost else { return store.replyByKey(item?.event, .accepted) }
                    let state: GhostState? = ghost.state == .going ? nil : .going
                    Task { await store.markGhost(ghost, state, viaKey: true) }
                    return true
                },
                KeyBinding(declining: "x", "dismiss") {
                    guard free() else { return false }
                    // A real invitation: no, through Google.
                    guard let ghost = item?.ghost else { return store.replyByKey(item?.event, .declined) }
                    Task { await store.markGhost(ghost, .dismissed, viaKey: true) }
                    return true
                },
            ]
        }

        // MARK: - helpers

        /// Every row j/k can reach, in the order drawn: folds count only
        /// while open.
        private func navigable(_ days: [AgendaDay]) -> [AgendaItem] {
            days.flatMap { day -> [AgendaItem] in
                let stamp = day.day.timeIntervalSince1970
                return (openFolds.contains("e\(stamp)") ? day.earlier : [])
                    + day.visible
                    + (openFolds.contains("u\(stamp)") ? day.unlikely : [])
            }
        }

        private func defaultSelection(
            _ next: (item: AgendaItem, ongoing: Bool)?, _ rows: [AgendaItem]
        ) -> AgendaItem? {
            if let next, rows.contains(where: { $0.id == next.item.id }) { return next.item }
            return rows.first
        }

        private func select(_ item: AgendaItem) {
            selection = item.id
        }

        private func toggle(_ key: String) {
            withAnimation(Motion.disclose) {
                if openFolds.contains(key) { openFolds.remove(key) } else { openFolds.insert(key) }
            }
        }

        /// Enter: a ghost opens the newest mail behind it; a real event opens
        /// in Google Calendar, in the browser. Declines when there is neither.
        private func open(_ item: AgendaItem) -> Bool {
            if let ghost = item.ghost, let source = ghost.sources.last {
                store.openThread(source.thread_id, focusMessage: source.message_id, from: .calendar)
                return true
            }
            if let link = item.event?.html_link, Opener.isHTTP(link) {
                Opener.open(link)
                return true
            }
            return false
        }

        private func spanText(_ item: AgendaItem, cal: Calendar) -> String {
            let start = Agenda.clock(item.start, cal)
            guard let end = item.end, end > item.start else { return start }
            return "\(start) to \(item.endEstimated ? "~" : "")\(Agenda.clock(end, cal))"
        }
    }

    // MARK: - refresh

    /// Reloads the agenda on every poll tick while the page is mounted, from
    /// a zero-size leaf so reading the tick invalidates nothing else. The
    /// first run (the page appearing) also asks the daemon to sync the
    /// calendar now.
    private struct CalendarRefreshKeeper: View {
        @Environment(AppStore.self) private var store
        @State private var kicked = false

        var body: some View {
            Color.clear
                .frame(width: 0, height: 0)
                .task(id: store.lastRefresh) {
                    // Also while a renewal is waiting: the sign it worked is
                    // a clean sync, and the daemon's own loop is five minutes
                    // away. The daemon throttles the kick to one per 30s.
                    let kick = !kicked || store.calendarLiveWait?.renewing == true
                    kicked = true
                    await store.refreshCalendar(kick: kick)
                }
        }
    }

    // MARK: - rows

    private struct AgendaRow: View {
        let item: AgendaItem
        let time: String
        let end: Agenda.EndLabel?
        let thin: Bool
        let selected: Bool
        let action: () -> Void

        /// The mark's diameter, an optical constant. The fold lines read it
        /// too, to start under the titles rather than under the mark.
        static let markSize: CGFloat = 10

        var body: some View {
            ListRow(selected: selected, hPadding: DeskRow.hPadding, vPadding: Space.scaled(5), action: action) {
                _, _ in
                HStack(alignment: .firstTextBaseline, spacing: Space.s10) {
                    Text(time)
                        .font(Typo.num(12))
                        .foregroundStyle(Palette.inkFaint)
                        .lineLimit(1)
                        .frame(width: Typo.measure(62), alignment: .trailing)
                    AgendaMark(
                        fill: item.fill, estimated: item.endEstimated,
                        thin: thin || item.cancelled, hue: ChipColors.inks(item).edge)
                        .frame(width: Self.markSize, height: Self.markSize)
                        // On the first line's middle, not its baseline.
                        .alignmentGuide(.firstTextBaseline) { $0[.bottom] - 1 }
                    HStack(alignment: .firstTextBaseline, spacing: Space.s6) {
                        // A commitment made in mail looks like a real event;
                        // the envelope is what says where it came from.
                        if item.isGhost, item.fill == .solid {
                            Image(systemName: "envelope")
                                .font(Typo.ui(9, weight: .medium))
                                .foregroundStyle(Palette.inkFaint)
                                .help("From your mail")
                        }
                        Text(item.title)
                            .font(Typo.row)
                            .foregroundStyle(Palette.ink)
                            .strikethrough(item.cancelled, color: Palette.inkFaint)
                            .lineLimit(1)
                            .layoutPriority(2)
                        if let place = item.location {
                            Text(place)
                                .font(Typo.rowSub)
                                .foregroundStyle(Palette.inkFaint)
                                .lineLimit(1)
                        }
                    }
                    Spacer(minLength: Space.s8)
                    meta
                        .layoutPriority(1)
                }
            }
            .help(item.title)
        }

        /// What the row says after the title, most important first: that it
        /// is off, when a guessed end lands, its band, where the mail came
        /// from. Words in the faint inks, never a chip: three chips a day made
        /// the mock's list read like a status board.
        private var meta: some View {
            HStack(alignment: .firstTextBaseline, spacing: Space.s8) {
                if item.cancelled {
                    Text("cancelled")
                        .font(Typo.micro)
                        .foregroundStyle(Palette.danger)
                }
                if let end {
                    EndText(label: end)
                }
                if let band = item.band, !item.cancelled {
                    Text(band.rawValue)
                        .font(Typo.micro)
                        .foregroundStyle(Palette.inkFaintest)
                }
                if let response = item.event?.self_response, response != .accepted {
                    Text(response == .tentative ? "tentative" : "no reply yet")
                        .font(Typo.micro)
                        .foregroundStyle(Palette.inkFaintest)
                }
                if item.unverified {
                    UnverifiedMark()
                }
                if item.pendingChange != nil {
                    ChangeOfferedMark()
                }
            }
            .lineLimit(1)
            .fixedSize()
        }
    }

    /// "3 unlikely ›" and "2 earlier today": a count that opens in place.
    private struct FoldLine: View {
        let text: String
        let open: Bool
        let action: () -> Void

        var body: some View {
            Button(action: action) {
                HStack(spacing: Space.s4) {
                    Text(text)
                    Image(systemName: "chevron.right")
                        .font(Typo.ui(8, weight: .semibold))
                        .rotationEffect(.degrees(open ? 90 : 0))
                }
                .font(Typo.micro)
                .padding(.vertical, Space.scaled(3))
                // Under the titles, past the time column and the mark.
                .padding(.leading, DeskRow.hPadding + Typo.measure(62) + Space.s10 * 2 + AgendaRow.markSize)
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            .buttonStyle(.textAction)
            .pointingHand()
        }
    }

    // MARK: - next and today

    /// What the user has to be at next. Blue, because "next" is state.
    private struct NextCard: View {
        let item: AgendaItem
        let lead: String
        let place: String?
        let span: String
        let selected: Bool
        let action: () -> Void

        var body: some View {
            Button(action: action) {
                VStack(alignment: .leading, spacing: Space.s2) {
                    Text(lead)
                        .font(Typo.ui(11, weight: .semibold))
                        .foregroundStyle(Palette.accentInk)
                    HStack(spacing: Space.scaled(7)) {
                        AgendaMark(
                            fill: item.fill, estimated: item.endEstimated, hue: ChipColors.inks(item).edge)
                            .frame(width: 10, height: 10)
                        Text(item.title)
                            .font(Typo.ui(15, weight: .semibold))
                            .foregroundStyle(Palette.ink)
                            .lineLimit(1)
                    }
                    Text([span, place].compactMap { $0 }.joined(separator: ", "))
                        .font(Typo.rowSub)
                        .foregroundStyle(Palette.inkDim)
                        .lineLimit(1)
                }
                // The rows' own inset, so the card's words sit on the page
                // gutter with theirs (the column is inset `DeskRow.listInset`).
                .padding(.horizontal, DeskRow.hPadding)
                .padding(.vertical, Space.s10)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(
                    RoundedRectangle(cornerRadius: Radius.well, style: .continuous)
                        .fill(Palette.accentSoft)
                )
                // Selection lights the edge EVENLY, all the way round.
                .overlay(
                    RoundedRectangle(cornerRadius: Radius.well, style: .continuous)
                        .strokeBorder(Palette.accent.opacity(selected ? 0.45 : 0), lineWidth: 1)
                )
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .pointingHand()
        }
    }

    /// Today's shape, 7am to midnight: how full the day is, in the same fills
    /// as the rows, with now as a blue tick. The rows say what; this says
    /// how much and when.
    private struct TodayStrip: View {
        let day: AgendaDay
        let now: Date
        let calendar: Calendar

        private static let height: CGFloat = 10

        var body: some View {
            let blocks = (day.earlier + day.visible).compactMap { item in
                Agenda.stripSpan(item, day: day.day, calendar: calendar).map { (item, $0) }
            }
            let tick = Agenda.stripNow(now, day: day.day, calendar: calendar)
            VStack(spacing: Space.s4) {
                GeometryReader { geo in
                    let w = geo.size.width
                    ZStack(alignment: .leading) {
                        RoundedRectangle(cornerRadius: 4, style: .continuous)
                            .fill(Palette.hairline.opacity(0.55))
                        ForEach(blocks, id: \.0.id) { item, span in
                            AgendaMark(
                                fill: item.fill, estimated: item.endEstimated,
                                thin: item.cancelled, hue: ChipColors.inks(item).edge, cornerRadius: 2.5)
                                .frame(width: max(3, w * (span.upperBound - span.lowerBound)))
                                .offset(x: w * span.lowerBound)
                        }
                        if let tick {
                            Capsule()
                                .fill(Palette.accent)
                                .frame(width: 2, height: Self.height + 6)
                                .offset(x: w * tick - 1)
                        }
                    }
                }
                .frame(height: Self.height)
                HStack {
                    Text("7am")
                    Spacer()
                    Text("noon")
                    Spacer()
                    Text("6pm")
                    Spacer()
                    Text("midnight")
                }
                .font(Typo.ui(9.5))
                .foregroundStyle(Palette.inkFaintest)
            }
            .padding(.horizontal, DeskRow.hPadding)
            .accessibilityElement()
            .accessibilityLabel("Today, 7am to midnight, \(blocks.count) blocks")
        }
    }

    // MARK: - the agenda, memoized

    /// The page's one costly derivation, kept until something it is built
    /// from changes: the feed, the minute, the zone. A reference held in
    /// `@State` and never observed, so filling it invalidates nothing.
    private final class AgendaMemo {
        private var feed: TimelineFeed?
        private var now: Date?
        private var zone: TimeZone?
        private var built: [AgendaDay] = []

        func days(_ feed: TimelineFeed?, now: Date, calendar: Calendar) -> [AgendaDay] {
            if now == self.now, calendar.timeZone == zone, feed == self.feed { return built }
            let items = feed.map { Agenda.items($0, calendar: calendar) } ?? []
            built = Agenda.days(items, now: now, calendar: calendar)
            (self.feed, self.now, zone) = (feed, now, calendar.timeZone)
            return built
        }
    }

    /// The month's derivations, kept until something they are built from
    /// changes: the feed and the zone (the items), the month (the grid), the
    /// day (the column). Not keyed by the clock: the grid does not move with
    /// the minute, and the now tick is drawn from `now` directly.
    private final class MonthMemo {
        private var feed: TimelineFeed?
        private var zone: TimeZone?
        private(set) var items: [AgendaItem] = []
        private var month: Date?
        private var firstWeekday: Int?
        private var provisional: AgendaItem?
        private var editing: TimelineEvent?
        private var built = MonthModel(month: .distantPast, weeks: [])
        private var day: Date?
        private var builtColumn: DayColumn?
        private var weekDay: Date?
        private var weekFirstWeekday: Int?
        private var builtWeek: WeekModel?

        /// The items and the draft's row, kept current: whatever was built
        /// from the ones before is dropped.
        private func sync(
            _ feed: TimelineFeed?, calendar: Calendar, provisional: AgendaItem?, editing: TimelineEvent?
        ) {
            if feed != self.feed || calendar.timeZone != zone {
                items = feed.map { Agenda.items($0, calendar: calendar) } ?? []
                (self.feed, zone) = (feed, calendar.timeZone)
                (builtColumn, month, builtWeek) = (nil, nil, nil)
            }
            // The event pane's draft is one more row, re-placed as it is
            // typed: the grid is rebuilt when the row itself changes (its
            // day, its times, its title, its colour), not on every keystroke
            // in the notes.
            if provisional != self.provisional || editing != self.editing {
                (self.provisional, self.editing) = (provisional, editing)
                (month, builtWeek) = (nil, nil)
            }
        }

        func month(
            _ feed: TimelineFeed?, month: Date, calendar: Calendar, provisional: AgendaItem? = nil,
            editing: TimelineEvent? = nil
        ) -> MonthModel {
            sync(feed, calendar: calendar, provisional: provisional, editing: editing)
            if month != self.month || calendar.firstWeekday != firstWeekday {
                built = MonthGrid.build(
                    EventForm.withProvisional(items, provisional, editing: editing), month: month,
                    calendar: calendar)
                (self.month, firstWeekday) = (month, calendar.firstWeekday)
            }
            return built
        }

        /// The week `day` is in, the draft's row among its blocks.
        func week(
            _ feed: TimelineFeed?, day: Date, calendar: Calendar, provisional: AgendaItem? = nil,
            editing: TimelineEvent? = nil
        ) -> WeekModel {
            sync(feed, calendar: calendar, provisional: provisional, editing: editing)
            if let builtWeek, day == weekDay, calendar.firstWeekday == weekFirstWeekday { return builtWeek }
            let model = WeekGrid.build(
                EventForm.withProvisional(items, provisional, editing: editing), day: day, calendar: calendar)
            (weekDay, weekFirstWeekday, builtWeek) = (day, calendar.firstWeekday, model)
            return model
        }

        /// Call after `month(_:month:calendar:)`, which keeps `items` current.
        func column(day: Date, calendar: Calendar) -> DayColumn {
            if let builtColumn, day == self.day { return builtColumn }
            let column = MonthGrid.column(items, day: day, calendar: calendar)
            (self.day, builtColumn) = (day, column)
            return column
        }
    }

    // MARK: - connecting and the sync

    /// Everything on the page that reads `stats`, and so changes on the
    /// poll's ten-second clock: the connect offer (and raising the card on a
    /// first visit), the wait, and the sync's health. A leaf, so a stats tick
    /// rebuilds these lines and not the agenda under them. Nothing drawn when
    /// there is nothing to say, and then it takes no room.
    private struct CalendarStatusLines: View {
        @Environment(AppStore.self) private var store

        var body: some View {
            // The wait has a deadline, so these lines keep a minute clock of
            // their own.
            TimelineView(.everyMinute) { context in
                lines(now: context.date)
            }
            .onChange(of: store.calendarOffer, initial: true) { _, offer in
                if offer == .card { store.raiseCalendarCardIfOwed() }
            }
        }

        @ViewBuilder
        private func lines(now: Date) -> some View {
            let wait = store.calendarLiveWait
            let waiting = store.calendarWaiting(now: now)
            let notice = CalendarGate.syncNotice(
                store.calendarFeed?.calendar, now: now, calendar: .current)
            VStack(spacing: 0) {
                // While the card is up it says all of this itself.
                if !store.calendarCardUp {
                    if waiting, let wait {
                        CalendarConnectBanner(waiting: true, renewing: wait.renewing)
                            .padding(.horizontal, Space.page)
                            .padding(.top, Space.s12)
                    } else if store.calendarOffer == .banner {
                        CalendarConnectBanner(waiting: false, renewing: false)
                            .padding(.horizontal, Space.page)
                            .padding(.top, Space.s12)
                    }
                }
                if !waiting, let notice {
                    CalendarSyncLine(notice: notice, canRenew: store.calendarRenewRoute != nil)
                        .padding(.horizontal, Space.page)
                        .padding(.top, Space.s12)
                }
            }
        }
    }

    /// The sync is not well. A renewal is the banner's shape with the
    /// banner's Connect, because it is the same door; a refresh that failed
    /// for Google's reasons is one faint line with nothing to press. Neither
    /// takes the danger or warning colour, and the events stay on screen.
    private struct CalendarSyncLine: View {
        @Environment(AppStore.self) private var store
        let notice: CalendarGate.SyncNotice
        let canRenew: Bool

        var body: some View {
            switch notice.kind {
            case .renew:
                HStack(spacing: Space.s10) {
                    Image(systemName: "calendar")
                        .font(Typo.ui(12))
                        .foregroundStyle(Palette.inkFaint)
                    Text(notice.text)
                        .font(Typo.rowSub)
                        .foregroundStyle(Palette.inkDim)
                    Spacer(minLength: Space.s8)
                    if canRenew {
                        Button("Connect") { store.openCalendarConnect(renewing: true) }
                            .buttonStyle(.glass)
                            .controlSize(.small)
                    }
                }
                // Two lines, not one: this sentence is longer than the
                // banner's, and cut off it would lose the reason for the button.
                .lineLimit(2)
                .padding(.horizontal, Space.s12)
                .padding(.vertical, Space.s6)
                .background(
                    RoundedRectangle(cornerRadius: Radius.well, style: .continuous)
                        .strokeBorder(Palette.hairline, lineWidth: 1)
                )
                .accessibilityElement(children: .combine)
            case .couldNotRefresh:
                Text(notice.text)
                    .font(Typo.micro)
                    .foregroundStyle(Palette.inkFaint)
                    .lineLimit(1)
                    // Plain words, so on the page gutter itself, the line the
                    // agenda's rows set their words on below it.
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
    }

    // MARK: - connect banner

    /// The calendar is not connected, said once and quietly, for an account
    /// that has seen the card. No tint and no warning colour: the page works
    /// without it, it just knows less. While Google's consent page is open it
    /// says it is waiting instead.
    private struct CalendarConnectBanner: View {
        @Environment(AppStore.self) private var store
        let waiting: Bool
        /// The wait is for a renewal: "Show again" raises the renewal's card.
        let renewing: Bool

        var body: some View {
            HStack(spacing: Space.s10) {
                if waiting {
                    WaitDots(tone: Palette.inkFaint)
                    Text("Waiting for Google to finish connecting your calendar.")
                        .font(Typo.rowSub)
                        .foregroundStyle(Palette.inkDim)
                } else {
                    Image(systemName: "calendar")
                        .font(Typo.ui(12))
                        .foregroundStyle(Palette.inkFaint)
                    Text("Showing plans found in your mail. Connect Google Calendar to add your events.")
                        .font(Typo.rowSub)
                        .foregroundStyle(Palette.inkDim)
                }
                Spacer(minLength: Space.s8)
                Button(waiting ? "Show again" : "Connect") {
                    store.openCalendarConnect(renewing: renewing)
                }
                    .buttonStyle(.glass)
                    .controlSize(.small)
            }
            .lineLimit(1)
            .padding(.horizontal, Space.s12)
            .padding(.vertical, Space.s6)
            .background(
                RoundedRectangle(cornerRadius: Radius.well, style: .continuous)
                    .strokeBorder(Palette.hairline, lineWidth: 1)
            )
            .accessibilityElement(children: .combine)
        }
    }
#endif
