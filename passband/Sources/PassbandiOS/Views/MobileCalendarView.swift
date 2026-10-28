// THE CALENDAR TAB. Behind the hidden `calendar` feature, it takes the bar's
// second slot (Quick Look's records move under the sitrep's board, see
// MobileSitrepView) because a calendar is somewhere you live and records are
// somewhere you look one fact up.
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

struct MobileCalendarView: View {
    @Environment(AppStore.self) private var store

    /// The strip is the whole month rather than one week.
    @State private var expanded = false
    /// Folds opened, by day: "u" + day for unlikely, "e" + day for earlier.
    @State private var openFolds: Set<String> = []
    /// The direction the strip last moved, for the slide.
    @State private var stepForward = true
    @State private var memo = MobileCalendarMemo()
    @State private var kicked = false
    #if DEBUG
        /// A screenshot's push (`--fixture-scene calendar-detail`).
        @State private var fixtureDetail: String?
    #endif

    var body: some View {
        // A minute clock: "Next, in 50 min", the earlier fold and today's dot
        // all move with it. The feed changes on the poll's cadence (below).
        TimelineView(.everyMinute) { context in
            let now = context.date
            let cal = Calendar.current
            let place = store.calendarMonthPlace(now: now, calendar: cal)
            let items = memo.items(store.calendarFeed, calendar: cal)
            let model = memo.month(items, month: place.month, calendar: cal)
            let days = Agenda.days(items, now: now, calendar: cal, from: place.day)
            let onToday = cal.isDate(place.day, inSameDayAs: now)
            let next = onToday ? Agenda.next(days, now: now) : nil
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 20) {
                    MobileCalendarStatus()
                    if let next {
                        MobileNextCard(
                            item: next.item,
                            lead: Agenda.nextLead(next.item, ongoing: next.ongoing, now: now, calendar: cal),
                            span: span(next.item, cal))
                    }
                    ForEach(days) { day in
                        daySection(day, now: now, cal: cal)
                    }
                    footnote(model: model, cal: cal)
                }
                .padding(.horizontal, 16)
                .padding(.top, 4)
                .padding(.bottom, 28)
                // A new day picked is a new list: it starts at its top.
                .id(place.day)
            }
            .scrollDismissesKeyboard(.never)
            .background(Palette.canvas)
            // THE STRIP RIDES THE BAR. A safe-area bar, not a pinned header:
            // the list scrolls under it with the platform's own edge effect,
            // the way it scrolls under the navigation bar above it.
            .safeAreaBar(edge: .top) {
                MobileCalendarStrip(
                    model: model, place: place, now: now, calendar: cal,
                    expanded: $expanded, stepForward: $stepForward)
            }
            .navigationTitle(MonthGrid.title(place.month, calendar: cal))
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    // Only off today: a Today that does nothing is a button
                    // that teaches you to stop pressing it.
                    if !onToday || !cal.isDate(place.month, equalTo: now, toGranularity: .month) {
                        Button("Today") {
                            Haptics.tick()
                            stepForward = place.day < cal.startOfDay(for: now)
                            withAnimation(.snappy) { store.moveCalendarMonth(.today) }
                        }
                        .foregroundStyle(Palette.accent)
                    }
                }
            }
        }
        .navigationDestination(for: MobileCalendarRoute.self) { route in
            MobileEventPage(id: route.id)
        }
        #if DEBUG
            .navigationDestination(item: $fixtureDetail) { id in MobileEventPage(id: id) }
            .task {
                if FixtureStage.on {
                    expanded = FixtureStage.calendarMonthOpen
                    try? await Task.sleep(for: .seconds(1.5))
                    fixtureDetail = FixtureStage.calendarDetail
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
            // The vocabulary is closed: the phone reports the mode it draws.
            Analytics.capture(
                "calendar_opened",
                ["granted": store.calendarGranted, "view": store.calendarMode.rawValue])
        }
    }

    // MARK: - a day

    @ViewBuilder
    private func daySection(_ day: AgendaDay, now: Date, cal: Calendar) -> some View {
        let title = Agenda.dayTitle(day.day, now: now, calendar: cal)
        let isToday = cal.isDate(day.day, inSameDayAs: now)
        let earlierKey = "e\(day.day.timeIntervalSince1970)"
        let unlikelyKey = "u\(day.day.timeIntervalSince1970)"
        VStack(alignment: .leading, spacing: 8) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Text(title.title)
                    .font(Typo.ui(15, weight: .semibold))
                    .foregroundStyle(isToday ? Palette.accentInk : Palette.ink)
                Text(title.detail)
                    .font(Typo.ui(13))
                    .foregroundStyle(Palette.inkFaint)
                Spacer(minLength: 0)
            }
            .padding(.horizontal, 4)
            .accessibilityAddTraits(.isHeader)

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

    /// Where the list stops. The request covers the strip's month plus the
    /// agenda's own window, so past that a quiet list is not an empty future.
    @ViewBuilder
    private func footnote(model: MonthModel, cal: Calendar) -> some View {
        if let last = model.days.last?.day, store.calendarLoaded {
            Text("Showing plans through \(Agenda.format(last, "MMM d", cal)).")
                .font(Typo.ui(12))
                .foregroundStyle(Palette.inkFaintest)
                .frame(maxWidth: .infinity, alignment: .center)
                .padding(.top, 4)
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
    @Binding var expanded: Bool
    @Binding var stepForward: Bool

    var body: some View {
        let weeks = expanded ? model.weeks : model.weeks.filter { week in
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
            handle
        }
        .padding(.horizontal, 12)
        .padding(.top, 4)
        .clipped()
        .contentShape(Rectangle())
        .gesture(swipe)
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

    /// Pull it open, push it shut: the month behind the week.
    private var handle: some View {
        Button {
            Haptics.tick()
            withAnimation(.snappy) { expanded.toggle() }
        } label: {
            Capsule()
                .fill(Palette.hairlineStrong)
                .frame(width: 36, height: 4)
                .frame(maxWidth: .infinity)
                .padding(.top, 2)
                .padding(.bottom, 6)
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(expanded ? "Show one week" : "Show the whole month")
    }

    /// Sideways turns the week (or the month); down opens the month, up
    /// closes it. Whichever way the finger mostly went wins.
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
                } else if abs(dy) > 40 {
                    let open = dy > 0
                    guard open != expanded else { return }
                    Haptics.tick()
                    withAnimation(.snappy) { expanded = open }
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
        let count = dots.count
        let things = count == 0 ? "" : count == 1 ? ", 1 plan" : ", \(count) or more plans"
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
            || response != nil || item.unverified
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
                } else if store.calendarOffer != .none {
                    // The Mac's first-visit card is a modal of its own; a
                    // phone says the same thing in the list, once per visit.
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

    private var item: AgendaItem? {
        guard let feed = store.calendarFeed else { return nil }
        return Agenda.items(feed, calendar: .current).first { $0.id == id }
    }

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
        .onChange(of: item == nil) { _, gone in
            if gone { dismiss() }
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
