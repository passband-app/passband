// NEW AND EDITED EVENTS ON THE PHONE: a sheet over the calendar tab, the
// Mac's event pane (Views/CalendarEventPane.swift) at a thumb's size.
//
// EVERYTHING BUT THE FIELDS IS THE MAC'S. The draft is `store.calendarDraft`
// (Lib/EventDraft.swift), opened by `store.openEventEditor`, saved by
// `store.saveEvent` through the same footer controls (`guestPlan`), and an
// answer that MAY have happened freezes this sheet exactly as it freezes the
// pane: read-only, with check again, send again (the same body, never the
// edited draft) and close. So the duplicate-event guarantees of #416 hold
// here without a line of their own.
//
// WHAT DIFFERS IS INPUT. The pane's wells are typed ("Oct 8", "7pm"); a phone
// has the platform's date and time wheels, which write the same strings
// (`EventForm.dateText` / `clockText`) and mark the field set by hand, so the
// title stops writing it. The title still reads itself on a new event
// ("Dinner with Sam fri 7pm" fills the day and the time, `EventForm.retitled`).
//
// NO GUESTS HERE YET. Inviting makes Google email real people, and the rules
// for when it may (`EventGuests`) deserve their own pass on a phone. An
// edited event keeps the guests it has; the save controls on offer still
// come from the plan, so "save and tell guests" appears exactly when the Mac
// would show it.

import SwiftUI

struct MobileEventSheet: View {
    @Environment(AppStore.self) private var store
    let request: CalendarEditorRequest

    @FocusState private var titleFocused: Bool
    @State private var choosing = false

    private var cal: Calendar { .current }

    var body: some View {
        let draft = store.calendarDraft ?? request.initial
        let now = Date()
        let state = store.calendarPaneState
        let editable = state.fieldsEditable && !store.calendarWriting
        let plan = store.guestPlan(draft, request)
        let problem = problem(draft, now: now)
        NavigationStack {
            Form {
                Section {
                    TextField("Title", text: titleBinding(now: now), axis: .vertical)
                        .font(Typo.ui(17, weight: .semibold))
                        .focused($titleFocused)
                        .submitLabel(.done)
                    if request.event == nil, !draft.title.isEmpty,
                        let reading = EventForm.titleReading(draft, today: now, calendar: cal), reading.filled,
                        reading.title != EventForm.trimmed(draft.title)
                    {
                        // What the title will be saved as, once the words
                        // that named the day and the time come out of it.
                        Text("Saves as \u{201C}\(reading.title)\u{201D}")
                            .font(Typo.ui(12))
                            .foregroundStyle(Palette.inkFaint)
                    }
                }
                Section {
                    Toggle("All day", isOn: field(\.allDay))
                    DatePicker(
                        draft.allDay ? "Starts" : "Day", selection: dayBinding(draft, now: now),
                        displayedComponents: .date)
                    if draft.allDay {
                        DatePicker(
                            "Ends", selection: endDayBinding(draft, now: now),
                            in: (EventForm.parseDate(draft.date, today: now, calendar: cal) ?? now)...,
                            displayedComponents: .date)
                    } else {
                        DatePicker(
                            "Starts", selection: timeBinding(draft, now: now, end: false),
                            displayedComponents: .hourAndMinute)
                        DatePicker(
                            "Ends", selection: timeBinding(draft, now: now, end: true),
                            displayedComponents: .hourAndMinute)
                    }
                } footer: {
                    if case .success(let when) = EventForm.when(draft, today: now, calendar: cal) {
                        Text(EventForm.summary(when, now: now, calendar: cal))
                    }
                }
                Section {
                    TextField("Place", text: field(\.location))
                    if store.calendarWritable.count > 1 || request.event == nil {
                        calendarPicker(draft)
                    }
                    TextField("Notes", text: field(\.notes), axis: .vertical)
                        .lineLimit(3...8)
                }
                if let problem, state == .editing {
                    Section {
                        Text(problem.text)
                            .font(Typo.ui(13))
                            .foregroundStyle(Palette.inkFaint)
                    }
                }
                if state.frozen {
                    frozen(state)
                }
            }
            .disabled(!editable)
            .scrollDismissesKeyboard(.interactively)
            .navigationTitle(request.event == nil ? "New event" : "Edit event")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { store.closeEventEditor() }
                        .disabled(store.calendarWriting && !state.frozen)
                }
                if !state.frozen {
                    ToolbarItem(placement: .confirmationAction) {
                        if store.calendarWriting {
                            ProgressView()
                        } else {
                            saveButton(draft, plan: plan, ready: problem == nil)
                        }
                    }
                }
            }
            .confirmationDialog("Tell the guests?", isPresented: $choosing, titleVisibility: .visible) {
                // Two ways to save an event with guests on it: the plan
                // offers both, and only the one that says so emails them.
                Button(sentence(plan.controls.primary.label)) { save(draft, via: plan.controls.primary) }
                if let quiet = plan.controls.quiet {
                    Button(sentence(quiet.label)) { save(draft, via: quiet) }
                }
            }
        }
        .interactiveDismissDisabled(store.eventDraftTouched || !editable)
        .task {
            // A new event starts on its title, keyboard up.
            if request.event == nil { titleFocused = true }
        }
    }

    // MARK: saving

    @ViewBuilder
    private func saveButton(_ draft: EventDraft, plan: GuestPlan, ready: Bool) -> some View {
        let primary = plan.controls.primary
        Button(plan.controls.quiet == nil ? sentence(primary.label) : "Save") {
            if plan.controls.quiet != nil {
                choosing = true
            } else {
                save(draft, via: primary)
            }
        }
        .fontWeight(.semibold)
        .disabled(!ready)
    }

    private func save(_ draft: EventDraft, via control: SaveControl) {
        Haptics.tick()
        Task {
            if await store.saveEvent(draft, request, via: control) {
                store.closeEventEditor()
            }
        }
    }

    /// "add event" on the Mac's footer, "Add event" as a button here.
    private func sentence(_ label: String) -> String {
        label.prefix(1).uppercased() + label.dropFirst()
    }

    private func problem(_ draft: EventDraft, now: Date) -> EventDraftProblem? {
        switch request.mode {
        case .create:
            return EventForm.problem(draft, today: now, calendar: cal)
        case .edit:
            if case .failure(let p) = EventForm.patch(draft, from: request.initial, today: now, calendar: cal) {
                return p
            }
            return nil
        }
    }

    /// Google may have made the change and the page has not shown it: the
    /// fields freeze, and only these three things are on offer.
    private func frozen(_ state: EventPaneState) -> some View {
        Section {
            Text(CalendarWrite.uncertainUnseenText)
                .font(Typo.ui(14))
                .foregroundStyle(Palette.inkDim)
            Button("Check again") {
                Task { if await store.checkEventAgain(request) { store.closeEventEditor() } }
            }
            Button("Send again") {
                Task { if await store.sendEventAgain(request) { store.closeEventEditor() } }
            }
            Button("Close", role: .cancel) { store.closeEventEditor() }
        }
        .disabled(state == .sending(again: true))
    }

    // MARK: the calendar

    private func calendarPicker(_ draft: EventDraft) -> some View {
        Picker("Calendar", selection: field(\.calendarId)) {
            ForEach(store.calendarWritable) { calendar in
                HStack {
                    Circle()
                        .fill(
                            CalendarColor.parse(calendar.color).map { ChipColors.inks(.calendar($0)).edge }
                                ?? Palette.inkFaint)
                        .frame(width: 10, height: 10)
                    Text(calendar.name ?? calendar.id)
                }
                .tag(Optional(calendar.id))
            }
        }
        .disabled(request.event != nil)
    }

    // MARK: bindings into the store's draft

    private func update(_ change: (inout EventDraft) -> Void) {
        guard var d = store.calendarDraft else { return }
        change(&d)
        if d != store.calendarDraft { store.calendarDraft = d }
    }

    private func field<T>(_ path: WritableKeyPath<EventDraft, T>) -> Binding<T> {
        Binding(
            get: { (store.calendarDraft ?? request.initial)[keyPath: path] },
            set: { value in update { $0[keyPath: path] = value } })
    }

    /// The title, read for a day and a time on a new event.
    private func titleBinding(now: Date) -> Binding<String> {
        Binding(
            get: { store.calendarDraft?.title ?? request.initial.title },
            set: { value in update { $0 = EventForm.retitled($0, title: value, today: now, calendar: cal) } })
    }

    private func day(_ draft: EventDraft, now: Date) -> Date {
        EventForm.parseDate(draft.date, today: now, calendar: cal) ?? cal.startOfDay(for: now)
    }

    /// The day wheel. Moving it moves an all-day run's end with it
    /// (`EventForm.moved`), and marks the day as the user's own.
    private func dayBinding(_ draft: EventDraft, now: Date) -> Binding<Date> {
        Binding(
            get: { day(draft, now: now) },
            set: { value in update { $0 = EventForm.moved($0, to: value, today: now, calendar: cal) } })
    }

    private func endDayBinding(_ draft: EventDraft, now: Date) -> Binding<Date> {
        let start = day(draft, now: now)
        return Binding(
            get: {
                let raw = EventForm.trimmed(draft.endDate)
                return raw.isEmpty
                    ? start : (EventForm.parseEndDate(raw, from: start, today: now, calendar: cal) ?? start)
            },
            set: { value in
                update {
                    $0 = EventForm.handSet($0, .date)
                    $0.endDate = cal.isDate(value, inSameDayAs: start)
                        ? "" : EventForm.dateText(cal.startOfDay(for: value), today: now, calendar: cal)
                }
            })
    }

    private func minutes(_ draft: EventDraft, end: Bool) -> Int {
        let start = EventForm.parseTime(draft.start)?.minutes ?? 9 * 60
        guard end else { return start }
        return EventForm.parseEnd(draft.end, after: start) ?? min(1439, start + 60)
    }

    /// A time wheel. The start keeps the length when it moves, the way a
    /// calendar app's does; the end is the end.
    private func timeBinding(_ draft: EventDraft, now: Date, end: Bool) -> Binding<Date> {
        let base = day(draft, now: now)
        return Binding(
            get: {
                cal.date(byAdding: .minute, value: minutes(draft, end: end) % 1440, to: base) ?? base
            },
            set: { value in
                let parts = cal.dateComponents([.hour, .minute], from: value)
                let m = (parts.hour ?? 0) * 60 + (parts.minute ?? 0)
                update { d in
                    if end {
                        d = EventForm.handSet(d, .end)
                        d.end = EventForm.clockText(m)
                    } else {
                        let length = (minutes(d, end: true) - minutes(d, end: false) + 1440) % 1440
                        d = EventForm.handSet(d, .start)
                        d.start = EventForm.clockText(m)
                        d.end = EventForm.clockText((m + max(15, length)) % 1440)
                    }
                }
            })
    }
}
