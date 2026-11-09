// THE CALENDAR TAB, the bar's second slot for every mailbox (Quick Look's
// records sit under the sitrep's board, see MobileSitrepView) because a
// calendar is somewhere you live and records are somewhere you look one fact
// up.
//
// NOT CONNECTED, IT ASKS. The first visit for an account that has not granted
// Google Calendar raises the Mac's own connect card over the tab (mounted by
// MobileShell, raised there on `calendarOffer == .card`), once per account;
// after that the inline card at the top of the list offers it, and its
// Connect raises the same card. Until then the tab shows the plans found in
// mail.
//
// THE SHAPE IS A PHONE'S, NOT A SMALLER MAC. The Mac opens on a month of
// top-down cells, which needs about 78 points a column; a phone has about 50,
// and a cell that small can hold a date and a few dots, not a title. So the
// grid here is the NAVIGATOR and the list under it is the content:
//   - A week strip pinned under the bar: the selected day's week, each day a
//     date with up to three dots in its calendars' colours. Swipe it for the
//     week before or after; pull the handle (or swipe down) and it opens into
//     the whole month, the same dots in every cell.
//   - Under it, the agenda FROM the selected day: that day always (empty or
//     not), then every day after it that has something on. Today's list
//     leads with the next commitment, as the Mac's agenda does.
//   - A row opens the same CalendarDetail the Mac shows beside its agenda,
//     pushed. Its verbs are the ghost's (going, dismiss, open the mail) and
//     Google Calendar's link; making and editing events stays on the Mac
//     until the phone has an event pane of its own.
//
// THE VISUAL LANGUAGE IS THE MAC'S, and it is binding (docs/calendar-ux-
// preview.html): fill is the likelihood scale (solid is real or committed,
// striped likely, an outline maybe, unlikely folds into a count); a ghost is
// an EMPTY container, never a faded one; a guessed end says "ends ~11pm" in
// the softer ink; blue is state (today, now, next), never a calendar.
//
// THE PLACE IS THE STORE'S. The month and the selected day are
// `store.calendarPlace`, the one the Mac's month uses, and the store's
// timeline request already covers whatever month that place is on. So
// turning the strip asks for the right range with no phone-only plumbing.

import SwiftUI

/// The phone's three ways of drawing the calendar, the desktop's switch at a
/// phone's size: the month's grid, one week's days, one day's hours. Not the
/// Mac's `CalendarViewMode`: the phone has no agenda mode (every mode lists),
/// and the Mac has no day mode (its month has a day column beside it).
enum PhoneCalendarMode: String, CaseIterable, Identifiable {
    case month, week, day

    var id: String { rawValue }
    /// The mode button's glyph: the shape of what is on screen.
    var symbol: String {
        switch self {
        case .month: "calendar"
        case .week: "list.bullet.below.rectangle"
        case .day: "clock"
        }
    }
    var label: String {
        switch self {
        case .month: "Month"
        case .week: "Week"
        case .day: "Day"
        }
    }
}

struct MobileCalendarView: View {
    @Environment(AppStore.self) private var store

    /// Kept across launches, like the Mac's mode: the way you like to look at
    /// your week is not a choice to make again every morning.
    /// Month the first time; after that, whatever was on screen last.
    @AppStorage("phoneCalendarMode") private var mode: PhoneCalendarMode = .month
    /// Folds opened, by day: "u" + day for unlikely, "e" + day for earlier.
    @State private var openFolds: Set<String> = []
    /// The direction the strip last moved, for the slide.
    @State private var stepForward = true
    @State private var memo = MobileCalendarMemo()
    @State private var kicked = false
    /// A search hit's detail, pushed once the store has turned to its day.
    @State private var searched: String?
    #if DEBUG
        /// A screenshot's push (`--fixture-scene calendar-detail`).
        @State private var fixtureDetail: String?
    #endif

    var body: some View {
        // A minute clock: "Next, in 50 min", the earlier fold, the now line and
        // today's dot all move with it. The feed changes on the poll's cadence.
        TimelineView(.everyMinute) { context in
            let now = context.date
            let cal = Calendar.current
            let place = store.calendarMonthPlace(now: now, calendar: cal)
            let items = memo.items(store.calendarFeed, calendar: cal)
            let model = memo.month(items, month: place.month, calendar: cal)
            let week = model.weeks.first { w in w.days.contains { cal.isDate($0.day, inSameDayAs: place.day) } }
            Group {
              if mode == .month {
                // THE MONTH IS THE WHOLE SCREEN, the way Google's is: every
                // day a cell with its plans in it. Picking a day opens it.
                VStack(spacing: 0) {
                    MobileCalendarStatus()
                        .padding(.horizontal, 16)
                    MobileMonthPager(
                        items: items, place: place, now: now, calendar: cal,
                        create: store.calendarCanCreate
                            ? { day in store.openEventEditor(day: day, now: now) } : nil,
                        open: { day in
                            Haptics.tick()
                            withAnimation(.snappy) {
                                store.selectCalendarDay(day, open: false, now: now)
                                mode = .day
                            }
                        })
                }
                .background(Palette.canvas)
              } else {
              ScrollViewReader { proxy in
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 20) {
                        MobileCalendarStatus()
                        content(
                            items: items, model: model, week: week, place: place, now: now, cal: cal,
                            scrollToFirstEvent: {
                                proxy.scrollTo(MobileDayTimeline.firstEventAnchor, anchor: .top)
                            })
                    }
                    .padding(.horizontal, 16)
                    .padding(.top, 4)
                    .padding(.bottom, 28)
                }
                // A new list starts at its top: a new day for the day's hours,
                // a new week for the week's. On the scroll view itself: a new
                // identity for its content alone keeps the old offset.
                .id(listKey(place: place, week: week))
                .scrollDismissesKeyboard(.never)
                .background(Palette.canvas)
                .onChange(of: place.day) { _, day in
                    // The week lists the day picked; bring it up.
                    if mode == .week {
                        withAnimation(.snappy) { proxy.scrollTo(day, anchor: .top) }
                    }
                }

              }
              }
            }
            // THE STRIP RIDES THE BAR. A safe-area bar, not a pinned header:
            // the list scrolls under it with the platform's own edge effect,
            // the way it scrolls under the navigation bar above it.
            .safeAreaBar(edge: .top) {
                VStack(spacing: 10) {
                    MobileCalendarStrip(
                        model: model, place: place, now: now, calendar: cal,
                        expanded: mode == .month, stepForward: $stepForward)
                }
            }
            // The month of what is on screen: the grid's month, or in Day and
            // Week the picked day's (which can be a greyed day of the next).
            .navigationTitle(MonthGrid.title(mode == .month ? place.month : place.day, calendar: cal))
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarLeading) {
                    modeButton
                }
                ToolbarItemGroup(placement: .topBarTrailing) {
                    // Only off today: a Today that does nothing is a button
                    // that teaches you to stop pressing it.
                    if !cal.isDate(place.day, inSameDayAs: now)
                        || !cal.isDate(place.month, equalTo: now, toGranularity: .month)
                    {
                        Button("Today") {
                            Haptics.tick()
                            stepForward = place.day < cal.startOfDay(for: now)
                            withAnimation(.snappy) { store.moveCalendarMonth(.today) }
                        }
                        .foregroundStyle(Palette.accent)
                    }
                    // A new event on the day picked (today's next hour on
                    // today), in the sheet. Only where the daemon lists a
                    // calendar the user can write to.
                    if store.calendarCanCreate {
                        Button {
                            Haptics.tick()
                            store.openEventEditor(day: place.day, now: now)
                        } label: {
                            Image(systemName: "plus")
                        }
                        .foregroundStyle(Palette.accent)
                        .accessibilityLabel("New event")
                    }
                }
            }
        }
        .navigationDestination(for: MobileCalendarRoute.self) { route in
            MobileEventPage(id: route.id)
        }
        // A HIT FROM SEARCH (MobileSearchView's Events block): the store has
        // turned to its day and named the item in `calendarSearchOpen`, a
        // one-shot ask written only by `openCalendarSearchHit`: the day's
        // hours, and the detail pushed over them. NOT `calendarMonthItem`,
        // which every save also sets (`landEvent`), so reading it pushed a
        // second page after each edit. Moving the place is what fetches that
        // month, so the page finds the row once the feed lands. `initial`
        // because the tab may mount on it.
        .navigationDestination(item: $searched) { id in MobileEventPage(id: id) }
        .onChange(of: store.calendarSearchOpen, initial: true) { _, id in
            guard let id else { return }
            store.calendarSearchOpen = nil
            mode = .day
            searched = id
        }
        // THE EVENT SHEET, on the store's own editor: what "+" opens, and the
        // detail's Edit from a page pushed on this stack.
        .sheet(item: editorBinding) { request in
            MobileEventSheet(request: request)
        }
        #if DEBUG
            .navigationDestination(item: $fixtureDetail) { id in MobileEventPage(id: id) }
            .task {
                if FixtureStage.on {
                    if let staged = FixtureStage.calendarMode.flatMap(PhoneCalendarMode.init(rawValue:)) {
                        mode = staged
                    }
                    if let offset = FixtureStage.calendarDay,
                       let day = Calendar.current.date(
                           byAdding: .day, value: offset, to: Calendar.current.startOfDay(for: Date())) {
                        store.selectCalendarDay(day, open: false, now: Date())
                    }
                    try? await Task.sleep(for: .seconds(1.5))
                    fixtureDetail = FixtureStage.calendarDetail
                    // The connect card, staged straight onto the store: the
                    // fixture inbox never offers it (`calendarOffer` is
                    // `.none` there), and a screenshot needs the ask.
                    if FixtureStage.scene == "calendar-connect" {
                        store.calendarConnectAsk = CalendarConnectAsk(
                            route: .link("https://example.invalid/reconnect?calendar=1"),
                            email: "you@example.com", epoch: store.epoch)
                    }
                    // The sheet, staged straight onto the store: the
                    // fixture inbox never offers "+" (nothing can be saved).
                    if FixtureStage.scene == "calendar-new" {
                        let draft = EventForm.retitled(
                            EventForm.newDraft(
                                day: Date(), now: Date(), calendarId: store.calendarWritable.first?.id,
                                calendar: .current),
                            title: "Dinner with Sam fri 7pm", today: Date(), calendar: .current)
                        store.calendarDraft = draft
                        store.calendarEditor = CalendarEditorRequest(
                            mode: .create, initial: draft, epoch: store.epoch)
                    }
                }
            }
        #endif
        // On every poll tick while the tab is mounted. The first run (the
        // tab appearing) also asks the daemon to sync the calendar now, and so
        // does a renewal still waiting on its first clean sync.
        .task(id: store.lastRefresh) {
            let kick = !kicked || store.calendarLiveWait?.renewing == true
            kicked = true
            await store.refreshCalendar(kick: kick)
        }
        // The month on screen is part of what is asked for, so turning the
        // strip into another month asks at once.
        .onChange(of: store.calendarRequestMonth(now: Date())) {
            Task { await store.refreshCalendar() }
        }
        .refreshable { await store.refreshCalendar(kick: true) }
        .onAppear {
            // The phone always draws a grid's place, so the request always
            // covers the strip's month (`calendarRequestMonth`).
            if !store.calendarMode.isGrid { store.setCalendarMode(.month) }
            // The vocabulary is closed: the phone reports the store's mode.
            Analytics.capture(
                "calendar_opened",
                ["granted": store.calendarGranted, "view": store.calendarMode.rawValue])
        }
    }

    /// THE DESKTOP'S SWITCH AS ONE GLYPH: it shows the mode you are in, and a
    /// tap steps to the next (Month, Week, Day, and round). Three pills spent
    /// a row of the screen on a choice made once a day.
    private var modeButton: some View {
        Button {
            Haptics.tick()
            let all = PhoneCalendarMode.allCases
            let next = all[((all.firstIndex(of: mode) ?? 0) + 1) % all.count]
            withAnimation(.snappy) { mode = next }
        } label: {
            Image(systemName: mode.symbol)
                .contentTransition(.symbolEffect(.replace))
        }
        .foregroundStyle(Palette.accent)
        .accessibilityLabel("Showing \(mode.label.lowercased())")
        .accessibilityHint("Switches to the next view")
    }

    /// The editor while it is this account's and not parked; dismissing the
    /// sheet closes it the way Cancel does.
    private var editorBinding: Binding<CalendarEditorRequest?> {
        Binding(
            get: { store.eventPaneOpen ? store.calendarEditor : nil },
            set: { if $0 == nil, store.calendarEditor != nil { store.closeEventEditor() } })
    }

    private func listKey(place: MonthPlace, week: MonthWeek?) -> String {
        switch mode {
        case .week: "w\(week?.id.timeIntervalSince1970 ?? 0)"
        case .month, .day: "\(mode.rawValue)\(place.day.timeIntervalSince1970)"
        }
    }

    @ViewBuilder
    private func content(
        items: [AgendaItem], model: MonthModel, week: MonthWeek?, place: MonthPlace, now: Date,
        cal: Calendar, scrollToFirstEvent: @escaping () -> Void
    ) -> some View {
        switch mode {
        case .month:
            // Drawn by MobileMonthGrid, never as a list.
            EmptyView()
        case .week:
            // Every day of the strip's week, a quiet one as a single line.
            let first = week?.days.first?.day ?? place.day
            let days = Agenda.run(items, now: now, calendar: cal, from: first, count: 7)
            let hasToday = days.contains { cal.isDate($0.day, inSameDayAs: now) }
            nextCard(hasToday ? days.filter { $0.day >= cal.startOfDay(for: now) } : [], now: now, cal: cal)
            ForEach(days) { day in
                daySection(day, now: now, cal: cal, compactEmpty: true)
                    .id(day.day)
            }
        case .day:
            MobileDayTimeline(
                column: MonthGrid.column(items, day: place.day, calendar: cal),
                now: now, calendar: cal,
                step: { forward in
                    stepForward = forward
                    Haptics.tick()
                    withAnimation(.snappy) { store.moveCalendarMonth(.days(forward ? 1 : -1), now: now) }
                },
                create: store.calendarCanCreate
                    ? { start in
                        Haptics.tick()
                        store.openEventEditor(
                            day: place.day, startMinutes: start, endMinutes: min(1440, start + 60), now: now)
                    } : nil,
                scrollToFirstEvent: scrollToFirstEvent)
        }
    }

    @ViewBuilder
    private func nextCard(_ days: [AgendaDay], now: Date, cal: Calendar) -> some View {
        if let next = Agenda.next(days, now: now) {
            MobileNextCard(
                item: next.item,
                lead: Agenda.nextLead(next.item, ongoing: next.ongoing, now: now, calendar: cal),
                span: span(next.item, cal))
        }
    }

    // MARK: - a day

    @ViewBuilder
    private func daySection(
        _ day: AgendaDay, now: Date, cal: Calendar, compactEmpty: Bool = false
    ) -> some View {
        let title = Agenda.dayTitle(day.day, now: now, calendar: cal)
        let isToday = cal.isDate(day.day, inSameDayAs: now)
        let earlierKey = "e\(day.day.timeIntervalSince1970)"
        let unlikelyKey = "u\(day.day.timeIntervalSince1970)"
        let quiet = compactEmpty && day.isEmpty
        VStack(alignment: .leading, spacing: 8) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Text(title.title)
                    .font(Typo.ui(15, weight: .semibold))
                    .foregroundStyle(isToday ? Palette.accentInk : Palette.ink)
                Text(title.detail)
                    .font(Typo.ui(13))
                    .foregroundStyle(Palette.inkFaint)
                Spacer(minLength: 0)
                // A quiet day in the week is this one line, no empty card.
                if quiet {
                    Text("Nothing on")
                        .font(Typo.ui(13))
                        .foregroundStyle(Palette.inkFaintest)
                }
            }
            .padding(.horizontal, 4)
            .accessibilityAddTraits(.isHeader)
            if !quiet {
                card(day, isToday: isToday, cal: cal, earlierKey: earlierKey, unlikelyKey: unlikelyKey)
            }
        }
    }

    @ViewBuilder
    private func card(
        _ day: AgendaDay, isToday: Bool, cal: Calendar, earlierKey: String, unlikelyKey: String
    ) -> some View {
        VStack(spacing: 0) {
            if day.isEmpty {
                Text(isToday ? "Nothing else on today." : "Nothing on.")
                    .font(Typo.ui(14))
                    .foregroundStyle(Palette.inkFaint)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.horizontal, 16)
                    .padding(.vertical, 14)
            }
            if !day.earlier.isEmpty {
                fold(
                    "\(day.earlier.count) earlier today", key: earlierKey,
                    first: true)
                if openFolds.contains(earlierKey) {
                    rows(day.earlier, on: day.day, thin: false, cal: cal, leadingRule: true)
                }
            }
            rows(day.visible, on: day.day, thin: false, cal: cal, leadingRule: !day.earlier.isEmpty)
            if !day.unlikely.isEmpty {
                fold(
                    "\(day.unlikely.count) unlikely", key: unlikelyKey,
                    first: day.earlier.isEmpty && day.visible.isEmpty)
                if openFolds.contains(unlikelyKey) {
                    rows(day.unlikely, on: day.day, thin: true, cal: cal, leadingRule: true)
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .passbandGlass(.pane, cornerRadius: Radius.pane, tint: Palette.glassTint)
    }

    @ViewBuilder
    private func rows(
        _ items: [AgendaItem], on day: Date, thin: Bool, cal: Calendar, leadingRule: Bool
    ) -> some View {
        ForEach(Array(items.enumerated()), id: \.element.id) { index, item in
            if index > 0 || leadingRule { Rule() }
            NavigationLink(value: MobileCalendarRoute(id: item.id)) {
                MobileAgendaRow(
                    item: item,
                    time: Agenda.timeLabel(item, on: day, calendar: cal),
                    end: Agenda.endLabel(item, on: day, calendar: cal),
                    thin: thin)
            }
            .buttonStyle(MobileCalendarRowStyle())
        }
    }

    /// "2 earlier today ›": a count that opens in place.
    private func fold(_ text: String, key: String, first: Bool) -> some View {
        let open = openFolds.contains(key)
        return VStack(spacing: 0) {
            if !first { Rule() }
            Button {
                Haptics.tick()
                withAnimation(.snappy) {
                    if open { openFolds.remove(key) } else { openFolds.insert(key) }
                }
            } label: {
                HStack(spacing: 5) {
                    Text(text)
                    Image(systemName: "chevron.right")
                        .font(Typo.ui(10, weight: .semibold))
                        .rotationEffect(.degrees(open ? 90 : 0))
                    Spacer(minLength: 0)
                }
                .font(Typo.ui(13, weight: .medium))
                .foregroundStyle(Palette.inkFaint)
                .padding(.leading, MobileAgendaRow.titleInset)
                .padding(.trailing, 16)
                .padding(.vertical, 11)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityValue(open ? "open" : "closed")
        }
    }

    private func span(_ item: AgendaItem, _ cal: Calendar) -> String {
        let full = Agenda.fullTime(item, calendar: cal)
        guard let end = full.end else { return full.lead }
        return "\(full.lead) to \(item.endEstimated ? "~" : "")\(end)"
    }
}

/// A row's push. The id, never the item: the page looks it up again in the
/// live feed, so a mark made there is drawn there, and a dismissal pops it.
struct MobileCalendarRoute: Hashable {
    let id: String
}

/// A hairline between rows, starting under the titles.
private struct Rule: View {
    var body: some View {
        Rectangle()
            .fill(Palette.hairline)
            .frame(height: 0.5)
            .padding(.leading, MobileAgendaRow.titleInset)
    }
}

/// A row pressed: the card's own wash, no shape of its own.
private struct MobileCalendarRowStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .background(Palette.hairline.opacity(configuration.isPressed ? 0.6 : 0))
            .contentShape(Rectangle())
    }
}

// MARK: - the strip

/// The week (or, opened, the month) under the bar: the navigator.
private struct MobileCalendarStrip: View {
    @Environment(AppStore.self) private var store
    let model: MonthModel
    let place: MonthPlace
    let now: Date
    let calendar: Calendar
    /// The month is on screen as its own grid: the weekday letters only.
    let expanded: Bool
    @Binding var stepForward: Bool

    var body: some View {
        // On the month the grid below IS the days: the strip is only their
        // letters.
        let weeks = expanded ? [] : model.weeks.filter { week in
            week.days.contains { calendar.isDate($0.day, inSameDayAs: place.day) }
        }
        VStack(spacing: 6) {
            weekdayHeader
            VStack(spacing: 2) {
                ForEach(weeks) { week in
                    weekRow(week)
                }
            }
            // A new week or month slides in from the side it came from.
            .id(expanded ? model.month : (weeks.first?.id ?? model.month))
            .transition(
                .asymmetric(
                    insertion: .move(edge: stepForward ? .trailing : .leading).combined(with: .opacity),
                    removal: .move(edge: stepForward ? .leading : .trailing).combined(with: .opacity)))
        }
        .padding(.horizontal, 12)
        .padding(.top, 2)
        .padding(.bottom, 8)
        .clipped()
        .contentShape(Rectangle())
        // Beside the day buttons, not under them: a plain gesture on the
        // parent loses every drag that starts on a button.
        .simultaneousGesture(swipe)
    }

    private var weekdayHeader: some View {
        HStack(spacing: 0) {
            ForEach(Array(Self.letters(calendar).enumerated()), id: \.offset) { _, letter in
                Text(letter)
                    .font(Typo.ui(11, weight: .medium))
                    .foregroundStyle(Palette.inkFaint)
                    .frame(maxWidth: .infinity)
            }
        }
        .accessibilityHidden(true)
    }

    /// One letter a weekday, from the user's first weekday.
    static func letters(_ calendar: Calendar) -> [String] {
        let names = ["S", "M", "T", "W", "T", "F", "S"]
        return (0..<7).map { names[(calendar.firstWeekday - 1 + $0) % 7] }
    }

    private func weekRow(_ week: MonthWeek) -> some View {
        HStack(spacing: 0) {
            ForEach(Array(week.days.enumerated()), id: \.element.id) { column, day in
                let covering = week.spans
                    .filter { $0.firstColumn <= column && column <= $0.lastColumn }
                    .map(\.item)
                DayCell(
                    day: day.day,
                    inMonth: day.inMonth || !expanded,
                    isToday: calendar.isDate(day.day, inSameDayAs: now),
                    selected: calendar.isDate(day.day, inSameDayAs: place.day),
                    dots: Array((covering + day.items).prefix(3)),
                    total: covering.count + day.items.count,
                    calendar: calendar
                ) {
                    Haptics.tick()
                    stepForward = day.day >= place.day
                    withAnimation(.snappy) {
                        store.selectCalendarDay(day.day, open: false, now: now)
                    }
                }
            }
        }
    }

    /// Sideways turns the week (or, on the month, the month).
    private var swipe: some Gesture {
        DragGesture(minimumDistance: 24)
            .onEnded { value in
                let dx = value.translation.width
                let dy = value.translation.height
                if abs(dx) > abs(dy), abs(dx) > 50 {
                    let forward = dx < 0
                    stepForward = forward
                    Haptics.tick()
                    withAnimation(.snappy) {
                        store.moveCalendarMonth(
                            expanded ? .months(forward ? 1 : -1) : .days(forward ? 7 : -7), now: now)
                    }
                }
            }
    }
}

/// One day in the strip: its date, and up to three dots.
private struct DayCell: View {
    let day: Date
    let inMonth: Bool
    let isToday: Bool
    let selected: Bool
    let dots: [AgendaItem]
    /// Everything on the day, of which `dots` are the first three.
    let total: Int
    let calendar: Calendar
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            VStack(spacing: 3) {
                Text(Agenda.format(day, "d", calendar))
                    .font(Typo.num(16, weight: isToday || selected ? .semibold : .regular))
                    .foregroundStyle(numberInk)
                    .frame(width: 34, height: 34)
                    .background {
                        if selected {
                            // Selection is ink; today's selection is blue,
                            // because today is state.
                            Circle().fill(isToday ? Palette.accent : Palette.ink)
                        }
                    }
                HStack(spacing: 3) {
                    ForEach(dots) { item in
                        Dot(item: item)
                    }
                }
                .frame(height: 5)
            }
            .frame(maxWidth: .infinity)
            .padding(.vertical, 2)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(accessibility)
        .accessibilityAddTraits(selected ? .isSelected : [])
    }

    private var numberInk: Color {
        if selected { return isToday ? .white : Palette.canvas }
        if isToday { return Palette.accent }
        return inMonth ? Palette.ink : Palette.inkFaintest
    }

    private var accessibility: String {
        let date = Agenda.format(day, "EEEE MMMM d", calendar)
        let things = total == 0 ? "" : total == 1 ? ", 1 plan" : ", \(total) plans"
        return (isToday ? "Today, " : "") + date + things
    }

    /// The fill scale at five points: solid is filled, anything less sure
    /// is the empty ring (stripes do not survive at this size).
    private struct Dot: View {
        let item: AgendaItem

        var body: some View {
            let hue = ChipColors.inks(item).edge
            Group {
                if item.fill == .solid, !item.cancelled {
                    Circle().fill(hue)
                } else {
                    Circle().strokeBorder(hue, lineWidth: 1)
                }
            }
            .frame(width: 5, height: 5)
        }
    }
}

// MARK: - rows

/// One plan in a day's card.
private struct MobileAgendaRow: View {
    let item: AgendaItem
    let time: String
    let end: Agenda.EndLabel?
    /// An unlikely row drawn out of its fold.
    let thin: Bool

    static let timeWidth: CGFloat = 58
    static let markSize: CGFloat = 10
    /// Where the titles start: the card's inset, the time column, the mark.
    static var titleInset: CGFloat { 14 + timeWidth + 10 + markSize + 10 }

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 10) {
            Text(time)
                .font(Typo.num(13))
                .foregroundStyle(Palette.inkDim)
                .lineLimit(1)
                .minimumScaleFactor(0.8)
                .frame(width: Self.timeWidth, alignment: .trailing)
            AgendaMark(
                fill: item.fill, estimated: item.endEstimated,
                thin: thin || item.cancelled, hue: ChipColors.inks(item).edge)
                .frame(width: Self.markSize, height: Self.markSize)
                // On the first line's middle, not its baseline.
                .alignmentGuide(.firstTextBaseline) { $0[.bottom] - 1 }
            VStack(alignment: .leading, spacing: 3) {
                HStack(alignment: .firstTextBaseline, spacing: 6) {
                    // A commitment made in mail looks like a real event; the
                    // envelope is what says where it came from.
                    if item.isGhost, item.fill == .solid {
                        Image(systemName: "envelope")
                            .font(Typo.ui(10, weight: .medium))
                            .foregroundStyle(Palette.inkFaint)
                            .accessibilityLabel("from your mail")
                    }
                    Text(item.title)
                        .font(Typo.ui(15, weight: .medium))
                        .foregroundStyle(Palette.ink)
                        .strikethrough(item.cancelled, color: Palette.inkFaint)
                        .lineLimit(2)
                        .multilineTextAlignment(.leading)
                }
                if hasSubline { subline }
            }
            Spacer(minLength: 0)
            Image(systemName: "chevron.right")
                .font(Typo.ui(11, weight: .semibold))
                .foregroundStyle(Palette.inkFaintest)
                .accessibilityHidden(true)
        }
        .padding(.leading, 14)
        .padding(.trailing, 14)
        .padding(.vertical, 12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .contentShape(Rectangle())
    }

    private var hasSubline: Bool {
        item.location != nil || item.cancelled || end != nil || (item.band != nil && !item.cancelled)
            || response != nil || item.unverified || item.pendingChange != nil
    }

    private var response: String? {
        guard let r = item.event?.self_response, r != .accepted else { return nil }
        return r == .tentative ? "tentative" : "no reply yet"
    }

    /// What the row says under the title, most important first: that it is
    /// off, when a guessed end lands, its band, where. Words in the faint
    /// inks, never chips.
    private var subline: some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            if item.cancelled {
                Text("cancelled")
                    .font(Typo.ui(12, weight: .medium))
                    .foregroundStyle(Palette.danger)
            }
            if let end {
                EndText(label: end)
            }
            if let band = item.band, !item.cancelled {
                Text(band.rawValue)
                    .font(Typo.micro)
                    .foregroundStyle(Palette.inkFaint)
            }
            if let response {
                Text(response)
                    .font(Typo.micro)
                    .foregroundStyle(Palette.inkFaint)
            }
            if item.unverified {
                UnverifiedMark()
            }
            if item.pendingChange != nil {
                ChangeOfferedMark()
            }
            if let place = item.location {
                Text(place)
                    .font(Typo.ui(12))
                    .foregroundStyle(Palette.inkFaint)
                    .lineLimit(1)
                    .truncationMode(.tail)
            }
        }
    }
}

/// What the user has to be at next. Blue, because "next" is state.
private struct MobileNextCard: View {
    let item: AgendaItem
    let lead: String
    let span: String

    var body: some View {
        NavigationLink(value: MobileCalendarRoute(id: item.id)) {
            VStack(alignment: .leading, spacing: 4) {
                Text(lead)
                    .font(Typo.ui(12, weight: .semibold))
                    .foregroundStyle(Palette.accentInk)
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    AgendaMark(fill: item.fill, estimated: item.endEstimated, hue: ChipColors.inks(item).edge)
                        .frame(width: 10, height: 10)
                        .alignmentGuide(.firstTextBaseline) { $0[.bottom] - 2 }
                    Text(item.title)
                        .font(Typo.ui(17, weight: .semibold))
                        .foregroundStyle(Palette.ink)
                        .lineLimit(2)
                        .multilineTextAlignment(.leading)
                }
                Text([span, item.location].compactMap { $0 }.joined(separator: ", "))
                    .font(Typo.ui(13))
                    .foregroundStyle(Palette.inkDim)
                    .lineLimit(2)
                    .multilineTextAlignment(.leading)
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 14)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(
                RoundedRectangle(cornerRadius: Radius.pane, style: .continuous)
                    .fill(Palette.accentSoft)
            )
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
    }
}

// MARK: - connect and sync

/// The calendar is not connected, or its sync is not well. Nothing drawn
/// when there is nothing to say. No tint and no warning colour: the tab works
/// without Google, it just knows less.
private struct MobileCalendarStatus: View {
    @Environment(AppStore.self) private var store

    var body: some View {
        TimelineView(.everyMinute) { context in
            let now = context.date
            let waiting = store.calendarWaiting(now: now)
            let notice = CalendarGate.syncNotice(store.calendarFeed?.calendar, now: now, calendar: .current)
            VStack(spacing: 12) {
                if waiting, let wait = store.calendarLiveWait {
                    card(
                        "Waiting for Google to finish connecting your calendar.",
                        button: "Show again", renewing: wait.renewing, dots: true)
                } else if store.calendarOffer != .none, !store.calendarCardUp {
                    // The first-visit card (MobileShell's overlay) says all
                    // of this itself while it is up; after it, the list says
                    // it here on every visit, and Connect raises the card.
                    card(
                        "Showing plans found in your mail. Connect Google Calendar to add your events.",
                        button: "Connect", renewing: false, dots: false)
                }
                if !waiting, let notice {
                    switch notice.kind {
                    case .renew:
                        card(
                            notice.text, button: store.calendarRenewRoute != nil ? "Connect" : nil,
                            renewing: true, dots: false)
                    case .couldNotRefresh:
                        Text(notice.text)
                            .font(Typo.ui(12))
                            .foregroundStyle(Palette.inkFaint)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .padding(.horizontal, 4)
                    }
                }
            }
        }
    }

    private func card(_ text: String, button: String?, renewing: Bool, dots: Bool) -> some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(alignment: .firstTextBaseline, spacing: 10) {
                if dots {
                    WaitDots(tone: Palette.inkFaint)
                } else {
                    Image(systemName: "calendar")
                        .font(Typo.ui(14))
                        .foregroundStyle(Palette.inkFaint)
                }
                Text(text)
                    .font(Typo.ui(14))
                    .foregroundStyle(Palette.inkDim)
                    .fixedSize(horizontal: false, vertical: true)
            }
            if let button {
                Button(button) { store.openCalendarConnect(renewing: renewing) }
                    .buttonStyle(.glass)
            }
        }
        .padding(16)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            RoundedRectangle(cornerRadius: Radius.pane, style: .continuous)
                .strokeBorder(Palette.hairline, lineWidth: 1)
        )
    }
}

// MARK: - one plan, pushed

/// A row opened: the Mac's own detail, on a page of its own. Looked up again
/// in the live feed by id, so "going" redraws here, and a dismissal (which
/// takes the row off the calendar) closes the page behind it.
private struct MobileEventPage: View {
    @Environment(AppStore.self) private var store
    @Environment(\.dismiss) private var dismiss
    let id: String
    /// A destructive answer was given and its delete is (or is about to be)
    /// out. Cleared when the write settles.
    @State private var answered = false

    private var item: AgendaItem? {
        guard let feed = store.calendarFeed else { return nil }
        return Agenda.items(feed, calendar: .current).first { $0.id == id }
    }

    /// The question is this page's while it is about this page's event.
    private var deleteWording: DeleteWording? {
        guard let ask = store.calendarDeleteAsk, ask.epoch == store.epoch, Agenda.itemId(ask.event) == id
        else { return nil }
        return EventGuests.deleteWording(ask.event, feed: store.calendarFeed)
    }

    /// THE QUESTION STAYS ASKED UNTIL IT IS SETTLED, as on the Mac: a delete
    /// answered here is out, and only `eventDeleted` clears the question (a
    /// failure leaves it up and the dialog comes back). SwiftUI writes false
    /// to this binding right after a button's action, before the delete's
    /// task has even started, so an answered question must not read that as
    /// "cancel": `answered` is set synchronously by the destructive buttons.
    private var deleteAsked: Binding<Bool> {
        Binding(
            get: { deleteWording != nil && !store.calendarWriting },
            set: { shown in
                guard !shown, !answered, store.calendarDeleteAsk != nil, !store.calendarWriting else { return }
                store.cancelDeleteEvent()
            })
    }

    private func sentence(_ s: String) -> String { s.prefix(1).uppercased() + s.dropFirst() }

    var body: some View {
        let item = item
        ScrollView {
            if let item {
                CalendarDetail(item: item, calendar: .current)
                    .padding(.horizontal, 16)
                    .padding(.top, 8)
                    .padding(.bottom, 28)
            }
        }
        .background(Palette.canvas)
        .navigationTitle("")
        .navigationBarTitleDisplayMode(.inline)
        // Mail opened from "Mail behind it" comes back here, not to the list.
        .readerHost()
        // THE DELETE QUESTION, asked where Delete was pressed: the store's
        // own (`askDeleteEvent`), worded by `EventGuests.deleteWording`, with
        // "tell guests" only where Google may tell them. Nothing is undone.
        .confirmationDialog(
            deleteWording?.title ?? "", isPresented: deleteAsked, titleVisibility: .visible
        ) {
            if let ask = store.calendarDeleteAsk, let words = deleteWording {
                if let tell = words.tell {
                    Button(sentence(tell), role: .destructive) {
                        answered = true
                        Task { await store.deleteEvent(ask.event, tellGuests: true) }
                    }
                }
                Button(sentence(words.quiet), role: .destructive) {
                    answered = true
                    Task { await store.deleteEvent(ask.event) }
                }
            }
            Button("Cancel", role: .cancel) {
                answered = false
                store.cancelDeleteEvent()
            }
        } message: {
            Text(deleteWording?.detail ?? "")
        }
        .onChange(of: store.calendarWriting) { _, writing in
            if !writing { answered = false }
        }
        .onChange(of: item == nil) { _, gone in
            guard gone else { return }
            // Gone under an open question (deleted elsewhere, say): the
            // question goes with the page, or every later "+" would be
            // refused by a question nobody can see.
            if deleteWording != nil, !store.calendarWriting { store.cancelDeleteEvent() }
            dismiss()
        }
    }
}

// MARK: - memo

/// The tab's costly derivations, kept until something they are built from
/// changes: the feed, the month, the zone. A reference held in `@State` and
/// never observed, so filling it invalidates nothing.
@MainActor
private final class MobileCalendarMemo {
    private var feed: TimelineFeed?
    private var zone: TimeZone?
    private var built: [AgendaItem] = []
    private var monthKey: (items: [AgendaItem], month: Date, zone: TimeZone)?
    private var monthBuilt: MonthModel?

    func items(_ feed: TimelineFeed?, calendar: Calendar) -> [AgendaItem] {
        if feed == self.feed, calendar.timeZone == zone { return built }
        self.feed = feed
        zone = calendar.timeZone
        built = feed.map { Agenda.items($0, calendar: calendar) } ?? []
        return built
    }

    func month(_ items: [AgendaItem], month: Date, calendar: Calendar) -> MonthModel {
        if let key = monthKey, let hit = monthBuilt, key.month == month, key.zone == calendar.timeZone,
            key.items == items
        {
            return hit
        }
        let model = MonthGrid.build(items, month: month, calendar: calendar)
        monthKey = (items, month, calendar.timeZone)
        monthBuilt = model
        return model
    }
}

// MARK: - the day's hours

/// One day as hours, 7am to midnight: the Mac's day column at a phone's width
/// (`MonthGrid.column`, the same lanes, the same night fold). All-day plans
/// and trips sit above the hours as chips; anything before 7am folds into one
/// line of its own rather than spending seven empty hours of scroll; unlikely
/// plans are a count. Swipe the hours sideways for the day before or after.
private struct MobileDayTimeline: View {
    let column: DayColumn
    let now: Date
    let calendar: Calendar
    let step: (_ forward: Bool) -> Void
    /// A tap on an empty stretch of the hours: a new event there, an hour
    /// long, starting on the half hour above the finger. Nil where nothing
    /// can be created (no writable calendar, the fixture inbox).
    var create: ((_ startMinutes: Int) -> Void)?
    /// Brings `firstEventAnchor` to the top of the list. Called by the mark
    /// itself as it appears, so it runs once the day's hours are laid out,
    /// whether the day was picked in the month, stepped to, or launched into.
    var scrollToFirstEvent: () -> Void = {}

    @State private var showUnlikely = false

    static let hourHeight: CGFloat = 56
    static let gutter: CGFloat = 48
    private var hours: Int { 24 - MonthGrid.dayFromHour }
    private var height: CGFloat { CGFloat(hours) * Self.hourHeight }

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            if !column.allDay.isEmpty {
                VStack(spacing: 6) {
                    ForEach(column.allDay) { item in
                        allDayChip(item)
                    }
                }
            }
            if !column.night.isEmpty {
                nightLine
            }
            // The mark beside the hours, in their frame.
            ZStack(alignment: .topLeading) {
                hoursWell
                    .simultaneousGesture(swipe)
                firstEventMark
            }
            if !column.unlikely.isEmpty {
                unlikelyFold
            }
        }
    }

    // MARK: above the hours

    private func allDayChip(_ item: AgendaItem) -> some View {
        let style = CalendarColor.style(item)
        let inks = ChipColors.inks(style.hue)
        let shape = RoundedRectangle(cornerRadius: 8, style: .continuous)
        let sub = Agenda.endLabel(item, on: column.day, calendar: calendar)?.text ?? "all day"
        return NavigationLink(value: MobileCalendarRoute(id: item.id)) {
            HStack(spacing: 8) {
                Text(item.title)
                    .font(Typo.ui(14, weight: .semibold))
                    .foregroundStyle(Palette.ink)
                    .strikethrough(style.struck, color: Palette.inkFaint)
                    .lineLimit(1)
                Spacer(minLength: 8)
                Text(sub)
                    .font(Typo.ui(12))
                    .foregroundStyle(Palette.inkDim)
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 9)
            .background(ChipFill(style: style, inks: inks, shape: shape))
            .contentShape(shape)
        }
        .buttonStyle(.plain)
    }

    /// Before 7am, said in one line: "5:30am  Flight to Denver".
    private var nightLine: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("Before \(Agenda.clock(dayStart, calendar))")
                .font(Typo.micro)
                .foregroundStyle(Palette.inkFaint)
                .textCase(.uppercase)
                .tracking(0.5)
            ForEach(column.night) { block in
                let item = block.segment.item
                NavigationLink(value: MobileCalendarRoute(id: item.id)) {
                    HStack(alignment: .firstTextBaseline, spacing: 10) {
                        AgendaMark(
                            fill: item.fill, estimated: item.endEstimated, thin: item.cancelled,
                            hue: ChipColors.inks(item).edge)
                            .frame(width: 10, height: 10)
                            .alignmentGuide(.firstTextBaseline) { $0[.bottom] - 1 }
                        Text(item.title)
                            .font(Typo.ui(14, weight: .medium))
                            .foregroundStyle(Palette.ink)
                            .lineLimit(1)
                        Spacer(minLength: 8)
                        Text(MonthGrid.rangeText(item, on: column.day, calendar: calendar))
                            .font(Typo.ui(12))
                            .foregroundStyle(Palette.inkDim)
                    }
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
            }
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            RoundedRectangle(cornerRadius: 12, style: .continuous)
                .strokeBorder(Palette.hairline, lineWidth: 1)
        )
    }

    private var dayStart: Date {
        calendar.date(byAdding: .hour, value: MonthGrid.dayFromHour, to: column.day) ?? column.day
    }

    // MARK: the hours

    /// What the day scrolls to when it opens (`MobileCalendarView`).
    static let firstEventAnchor = "day-first-event"
    /// Hours of the well always on screen when a day opens, so a first
    /// event inside them needs no scroll and the all-day chips stay in view.
    static let opensOnScreen = 4.0
    /// How much of the hours above the first event the scroll keeps.
    static let leadHours = 1.5

    /// A mark an hour and a half above the day's first timed event, when that
    /// event starts too far down the well to be seen as the day opens. No
    /// mark (no scroll) for a day whose first event is near the top, or with
    /// no timed event at all.
    @ViewBuilder private var firstEventMark: some View {
        if let first = column.blocks.map(\.segment.lo).min() {
            let top = CGFloat(first) * height
            if top > CGFloat(Self.opensOnScreen) * Self.hourHeight {
                // A spacer, then the mark, in a stack: the mark's OWN frame
                // starts at the hour. An offset moves only the drawing, and a
                // padded mark was found by `scrollTo` at the padding's top
                // (both seen in the simulator, where the day scrolled only to
                // its top).
                VStack(spacing: 0) {
                    Color.clear
                        .frame(width: 1, height: max(0, top - CGFloat(Self.leadHours) * Self.hourHeight))
                    Color.clear
                        .frame(width: 1, height: 1)
                        .id(Self.firstEventAnchor)
                        .onAppear {
                            // The next turn of the run loop: the list's own
                            // layout pass has to have placed the mark first.
                            DispatchQueue.main.async { scrollToFirstEvent() }
                        }
                }
                .accessibilityHidden(true)
            }
        }
    }

    private var hoursWell: some View {
        GeometryReader { geo in
            let lane = max(0, geo.size.width - Self.gutter - 4)
            ZStack(alignment: .topLeading) {
                // The empty hours, under everything: a tap here is a new
                // event at that time. Blocks sit above it and take their own.
                Color.clear
                    .contentShape(Rectangle())
                    .onTapGesture { point in
                        guard let create else { return }
                        let hours = max(0, Double(point.y) / Double(Self.hourHeight))
                        let minutes = MonthGrid.dayFromHour * 60 + Int(hours * 2) * 30
                        // An hour long, always: the last start is 11pm.
                        create(min(23 * 60, minutes))
                    }
                // The hour lines, each labelled in the gutter. Midnight closes
                // the well without a label of its own.
                ForEach(0...hours, id: \.self) { i in
                    let y = CGFloat(i) * Self.hourHeight
                    if i < hours {
                        Text(hourLabel(MonthGrid.dayFromHour + i))
                            .font(Typo.num(11))
                            .foregroundStyle(Palette.inkFaint)
                            .frame(width: Self.gutter - 8, alignment: .trailing)
                            .offset(y: y - 7)
                    }
                    Rectangle()
                        .fill(Palette.hairline)
                        .frame(width: lane + 4, height: 0.5)
                        .offset(x: Self.gutter, y: y)
                }
                ForEach(visualBlocks) { block in
                    blockView(block, laneWidth: lane)
                }
                nowLine(width: lane)
            }
        }
        .frame(height: height + 8)
        .accessibilityElement(children: .contain)
    }

    /// The blocks laid out by the height they are DRAWN at, not their
    /// minutes: a 15-minute event is drawn 24 points tall, so two back to
    /// back would stack one over the other's title if they shared a lane.
    private var visualBlocks: [ColumnBlock] {
        let minimum = Double(Self.minBlock / height)
        let widened = column.blocks.map { b -> DaySegment in
            let s = b.segment
            return DaySegment(
                item: s.item, part: s.part, lo: min(s.lo, 1 - minimum), hi: max(s.hi, min(1, s.lo + minimum)),
                continuesBefore: s.continuesBefore, continuesAfter: s.continuesAfter)
        }
        return MonthGrid.lanes(widened)
    }

    static let minBlock: CGFloat = 24

    private func hourLabel(_ hour: Int) -> String {
        let h = hour % 12 == 0 ? 12 : hour % 12
        return hour == 12 ? "noon" : "\(h)\(hour < 12 ? "am" : "pm")"
    }

    private func blockView(_ block: ColumnBlock, laneWidth: CGFloat) -> some View {
        let segment = block.segment
        let item = segment.item
        let width = laneWidth / CGFloat(max(1, block.lanes))
        let top = CGFloat(segment.lo) * height
        let h = max(Self.minBlock, CGFloat(segment.hi - segment.lo) * height - 2)
        let style = CalendarColor.style(item)
        let inks = ChipColors.inks(style.hue)
        let r: CGFloat = 8
        let shape = UnevenRoundedRectangle(
            topLeadingRadius: segment.continuesBefore ? 0 : r,
            bottomLeadingRadius: segment.continuesAfter ? 0 : r,
            bottomTrailingRadius: segment.continuesAfter ? 0 : r,
            topTrailingRadius: segment.continuesBefore ? 0 : r, style: .continuous)
        let roomy = h >= 44
        return NavigationLink(value: MobileCalendarRoute(id: item.id)) {
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 4) {
                    if item.isGhost, item.fill == .solid {
                        Image(systemName: "envelope")
                            .font(Typo.ui(10, weight: .medium))
                            .foregroundStyle(inks.ink)
                    }
                    Text(item.title)
                        .font(Typo.ui(13, weight: item.fill == .outline ? .medium : .semibold))
                        .foregroundStyle(Palette.ink)
                        .strikethrough(style.struck, color: Palette.inkFaint)
                        .lineLimit(roomy ? 2 : 1)
                }
                if roomy {
                    Text(
                        [MonthGrid.rangeText(item, on: column.day, calendar: calendar), item.location]
                            .compactMap { $0 }.joined(separator: ", ")
                    )
                    .font(Typo.ui(11))
                    .foregroundStyle(Palette.inkDim)
                    .lineLimit(1)
                }
            }
            .padding(.horizontal, 8)
            .padding(.top, roomy ? 6 : 3)
            .frame(width: max(0, width - 3), height: h, alignment: .topLeading)
            .background(ChipFill(style: style, inks: inks, shape: shape))
            .clipShape(shape)
            .contentShape(shape)
        }
        .buttonStyle(.plain)
        .offset(x: Self.gutter + 4 + CGFloat(block.lane) * width, y: top + 1)
        .accessibilityLabel("\(item.title), \(MonthGrid.rangeText(item, on: column.day, calendar: calendar))")
    }

    /// Now, on today only: a blue line with a dot at the gutter, blue being
    /// state.
    @ViewBuilder
    private func nowLine(width: CGFloat) -> some View {
        if let mark = MonthGrid.nowMark(now, day: column.day, calendar: calendar), mark.part == .day {
            let y = CGFloat(mark.at) * height
            HStack(spacing: 0) {
                Circle().fill(Palette.accent).frame(width: 8, height: 8)
                Rectangle().fill(Palette.accent).frame(width: width, height: 1.5)
            }
            .offset(x: Self.gutter - 4, y: y - 4)
            .allowsHitTesting(false)
            .accessibilityHidden(true)
        }
    }

    private var unlikelyFold: some View {
        VStack(alignment: .leading, spacing: 8) {
            Button {
                Haptics.tick()
                withAnimation(.snappy) { showUnlikely.toggle() }
            } label: {
                HStack(spacing: 5) {
                    Text("\(column.unlikely.count) unlikely")
                    Image(systemName: "chevron.right")
                        .font(Typo.ui(10, weight: .semibold))
                        .rotationEffect(.degrees(showUnlikely ? 90 : 0))
                }
                .font(Typo.ui(13, weight: .medium))
                .foregroundStyle(Palette.inkFaint)
            }
            .buttonStyle(.plain)
            if showUnlikely {
                ForEach(column.unlikely) { item in
                    NavigationLink(value: MobileCalendarRoute(id: item.id)) {
                        HStack(alignment: .firstTextBaseline, spacing: 10) {
                            AgendaMark(fill: item.fill, thin: true, hue: ChipColors.inks(item).edge)
                                .frame(width: 10, height: 10)
                                .alignmentGuide(.firstTextBaseline) { $0[.bottom] - 1 }
                            Text(item.title)
                                .font(Typo.ui(14))
                                .foregroundStyle(Palette.ink)
                            Spacer(minLength: 8)
                            Text(Agenda.timeLabel(item, on: column.day, calendar: calendar))
                                .font(Typo.ui(12))
                                .foregroundStyle(Palette.inkDim)
                        }
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                }
            }
        }
        .padding(.horizontal, 4)
    }

    private var swipe: some Gesture {
        DragGesture(minimumDistance: 24)
            .onEnded { value in
                let dx = value.translation.width
                guard abs(dx) > abs(value.translation.height), abs(dx) > 60 else { return }
                step(dx < 0)
            }
    }
}

// MARK: - the month, whole

/// The month as Google's phone app draws it, and as the Mac's month does
/// (`MonthGrid.build`, `fit`, `fold`): every day a cell, its plans top-down
/// as small chips in their calendars' colours, trips as bars across the days
/// they cover, and "+N" when a day runs out of room. The cells share the
/// screen's height, so a five-week month has taller days than a six-week one.
/// A day tapped opens in Day. One page of `MobileMonthPager`.
private struct MobileMonthGrid: View {
    let items: [AgendaItem]
    let month: Date
    let now: Date
    let calendar: Calendar
    /// Press and hold a day: a new event on it.
    var create: ((Date) -> Void)?
    let open: (Date) -> Void

    static let numberBand: CGFloat = 24
    static let rowHeight: CGFloat = 15
    static let rowGap: CGFloat = 2
    static let foot: CGFloat = 3
    static let inset: CGFloat = 2
    /// A cell never shrinks past a date and two rows; a month that cannot fit
    /// at that height scrolls.
    static var minCell: CGFloat { numberBand + 2 * (rowHeight + rowGap) + foot }

    var body: some View {
        let model = MonthGrid.build(items, month: month, calendar: calendar)
        GeometryReader { geo in
            let rows = max(1, model.weeks.count)
            let cell = max(Self.minCell, floor(geo.size.height / CGFloat(rows)))
            let width = geo.size.width / 7
            ScrollView {
                VStack(spacing: 0) {
                    ForEach(model.weeks) { week in
                        weekRow(week, cellHeight: cell, cellWidth: width)
                    }
                }
            }
            .scrollDisabled(cell * CGFloat(rows) <= geo.size.height + 1)
            .scrollBounceBehavior(.basedOnSize)
        }
        .clipped()
    }

    private func weekRow(_ week: MonthWeek, cellHeight: CGFloat, cellWidth: CGFloat) -> some View {
        let slots = MonthGrid.slots(
            cellHeight: Double(cellHeight), lanes: 0, numberBand: Double(Self.numberBand),
            rowHeight: Double(Self.rowHeight), rowGap: Double(Self.rowGap), foot: Double(Self.foot))
        let fit = MonthGrid.fit(week, slots: slots)
        return ZStack(alignment: .topLeading) {
            HStack(spacing: 0) {
                ForEach(Array(week.days.enumerated()), id: \.element.id) { column, day in
                    cellView(
                        day, rows: fit.rows[column],
                        fold: MonthGrid.fold(
                            rows: fit.rows[column],
                            barLanes: MonthGrid.barLanes(fit.bars, column: column),
                            hiddenSpans: fit.overflow[column], slots: slots),
                        spans: week.spans
                            .filter { $0.firstColumn <= column && column <= $0.lastColumn }
                            .map(\.item),
                        width: cellWidth, height: cellHeight)
                }
            }
            // The trips, over the cells they cross, in their lanes.
            ForEach(fit.bars) { bar in
                spanBar(bar, cellWidth: cellWidth)
            }
        }
        .frame(height: cellHeight, alignment: .top)
        .overlay(alignment: .top) {
            Rectangle().fill(Palette.hairline).frame(height: 0.5)
        }
    }

    private func y(_ row: Int) -> CGFloat {
        Self.numberBand + CGFloat(row) * (Self.rowHeight + Self.rowGap)
    }

    private func cellView(
        _ day: MonthDay, rows: [Int], fold: CellFold, spans: [AgendaItem], width: CGFloat, height: CGFloat
    ) -> some View {
        let isToday = calendar.isDate(day.day, inSameDayAs: now)
        return Button {
            open(day.day)
        } label: {
            ZStack(alignment: .topLeading) {
                Text(Agenda.format(day.day, "d", calendar))
                    .font(Typo.num(12, weight: isToday ? .semibold : .medium))
                    .foregroundStyle(
                        isToday ? Color.white : day.inMonth ? Palette.ink : Palette.inkFaintest)
                    .frame(width: 22, height: 22)
                    .background {
                        if isToday { Circle().fill(Palette.accent) }
                    }
                    .frame(maxWidth: .infinity)
                    .padding(.top, 1)
                ForEach(Array(zip(day.items.prefix(fold.shown), rows.prefix(fold.shown))), id: \.0.id) {
                    item, row in
                    chip(item, sub: nil, width: width - 2 * Self.inset, flatLeading: false, flatTrailing: false)
                        .offset(x: Self.inset, y: y(row))
                }
                if let moreRow = fold.moreRow, fold.more > 0 {
                    Text("+\(fold.more)")
                        .font(Typo.ui(10, weight: .semibold))
                        .foregroundStyle(Palette.inkFaint)
                        .frame(height: Self.rowHeight)
                        .offset(x: Self.inset + 3, y: y(moreRow))
                }
            }
            .frame(width: width, height: height, alignment: .topLeading)
            .opacity(day.inMonth ? 1 : 0.55)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .overlay(alignment: .leading) {
            Rectangle().fill(Palette.hairline).frame(width: 0.5)
        }
        .accessibilityLabel(accessibility(day, spans: spans, isToday: isToday))
        .contextMenu {
            if let create {
                Button { create(day.day) } label: { Label("New event", systemImage: "plus") }
            }
            Button { open(day.day) } label: { Label("Open day", systemImage: "clock") }
        }
    }

    private func spanBar(_ bar: SpanSegment, cellWidth: CGFloat) -> some View {
        let label = MonthGrid.spanLabel(bar, calendar: calendar)
        let lead: CGFloat = bar.startsHere ? Self.inset : 0
        let trail: CGFloat = bar.endsHere ? Self.inset : 0
        let width = CGFloat(bar.lastColumn - bar.firstColumn + 1) * cellWidth - lead - trail
        return chip(bar.item, sub: label.sub, width: width, flatLeading: !bar.startsHere, flatTrailing: !bar.endsHere)
            .offset(x: CGFloat(bar.firstColumn) * cellWidth + lead, y: y(bar.lane))
            .allowsHitTesting(false)
            // Each day's own label names the trips over it.
            .accessibilityHidden(true)
    }

    /// One plan in a cell: its calendar's fill, its title clipped rather than
    /// ellipsized (at 50 points "Lunch w" says more than "Lu..."). A timed
    /// plan keeps the ink of its hue on a lighter fill, like the Mac's chip.
    private func chip(
        _ item: AgendaItem, sub: String?, width: CGFloat, flatLeading: Bool, flatTrailing: Bool
    ) -> some View {
        let style = CalendarColor.style(item)
        let inks = ChipColors.inks(style.hue)
        let r: CGFloat = 4
        let shape = UnevenRoundedRectangle(
            topLeadingRadius: flatLeading ? 0 : r, bottomLeadingRadius: flatLeading ? 0 : r,
            bottomTrailingRadius: flatTrailing ? 0 : r, topTrailingRadius: flatTrailing ? 0 : r,
            style: .continuous)
        return HStack(spacing: 3) {
            Text(item.title)
                .font(Typo.ui(10, weight: item.fill == .outline ? .medium : .semibold))
                .foregroundStyle(Palette.ink)
                .strikethrough(style.struck, color: Palette.inkFaint)
            if let sub {
                Text(sub)
                    .font(Typo.ui(9.5))
                    .foregroundStyle(Palette.inkDim)
            }
        }
        .lineLimit(1)
        .fixedSize()
        .padding(.horizontal, 3)
        .frame(width: max(0, width), height: Self.rowHeight, alignment: .leading)
        .background(ChipFill(style: style, inks: inks, shape: shape, stripeSpacing: 4))
        .clipShape(shape)
    }

    private func accessibility(_ day: MonthDay, spans: [AgendaItem], isToday: Bool) -> String {
        let date = Agenda.format(day.day, "EEEE MMMM d", calendar)
        // Everything on the day, spoken whole: the trips over it, then its
        // own plans, including what the "+N" folded away.
        let titles = (spans + day.items).map(\.title)
        let plans = titles.isEmpty ? "" : ", " + titles.joined(separator: ", ")
        return (isToday ? "Today, " : "") + date + plans
    }
}

// MARK: - the months, paged

/// THE MONTHS AS PAGES: the whole grid follows the thumb and the next month
/// slides in beside it, the way a photo does, and lets go onto a page. The
/// platform's own pager (a page-style TabView), not a drag hand-rolled over
/// one grid: that one could only jump once the finger lifted.
///
/// Ten years either side of this month. The page settled on IS the store's
/// month (`moveCalendarMonth`), so the title, the request for that month's
/// plans and the other modes all follow it; Today, or a day picked in Week,
/// moves the pager the other way. A page off the settled one draws what the
/// feed already holds for it, and fills in when it lands and its month is
/// asked for.
private struct MobileMonthPager: View {
    @Environment(AppStore.self) private var store
    let items: [AgendaItem]
    let place: MonthPlace
    let now: Date
    let calendar: Calendar
    var create: ((Date) -> Void)?
    let open: (Date) -> Void

    /// Pages are ABSOLUTE month numbers (year * 12 + month), never offsets
    /// from today: an offset meant something else after midnight on the
    /// 31st, and the pager drifted off the store's month.
    @State private var page: Int
    /// The middle of the range, fixed when the pager is made.
    @State private var anchor: Int

    static let reach = 60

    init(
        items: [AgendaItem], place: MonthPlace, now: Date, calendar: Calendar,
        create: ((Date) -> Void)? = nil, open: @escaping (Date) -> Void
    ) {
        self.items = items
        self.place = place
        self.now = now
        self.calendar = calendar
        self.create = create
        self.open = open
        // Seeded here, not in onAppear: a page-style TabView can ignore (or
        // write back over) a selection changed before its first layout.
        let start = Self.number(place.month, calendar)
        _page = State(initialValue: start)
        _anchor = State(initialValue: start)
    }

    static func number(_ month: Date, _ calendar: Calendar) -> Int {
        let c = calendar.dateComponents([.year, .month], from: month)
        return (c.year ?? 2000) * 12 + (c.month ?? 1) - 1
    }

    private func month(_ number: Int) -> Date {
        var c = DateComponents()
        c.year = number / 12
        c.month = number % 12 + 1
        c.day = 1
        return calendar.date(from: c) ?? place.month
    }

    var body: some View {
        TabView(selection: $page) {
            ForEach((anchor - Self.reach)...(anchor + Self.reach), id: \.self) { number in
                // The grid builds its own month, in its own body: only the
                // pages the pager realises pay for MonthGrid.build.
                MobileMonthGrid(
                    items: items, month: month(number), now: now, calendar: calendar,
                    create: create, open: open
                )
                .tag(number)
            }
        }
        .tabViewStyle(.page(indexDisplayMode: .never))
        // Settled on a page: that month becomes the store's.
        .onChange(of: page) { _, new in
            let current = Self.number(place.month, calendar)
            guard new != current else { return }
            Haptics.tick()
            store.moveCalendarMonth(.months(new - current), now: now)
        }
        // The month moved from elsewhere (Today, another mode): follow it.
        .onChange(of: place.month) { _, moved in
            let target = Self.number(moved, calendar)
            if target != page { withAnimation(.snappy) { page = target } }
        }
    }
}
