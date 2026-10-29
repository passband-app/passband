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
// GUESTS ARE THE MAC'S RULES. Whether the line is a field, closed (and why),
// or absent is `EventGuests.plan`; the controls it offers are the only ones
// drawn, and only a control that says so ("Add and invite 2", "Save and tell
// guests") makes Google email anyone. Typing an address never invites: the
// button does.

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
                Group {
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
                        Toggle("All day", isOn: allDayBinding(now: now))
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
                    guests(draft, plan: plan)
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
                }
                // The FIELDS freeze, never the frozen controls: those sit
                // outside this, or a 504 would leave nothing to press but
                // Cancel (and a second add would make a duplicate).
                .disabled(!editable)
                if state.frozen {
                    frozen(state)
                }
            }
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
        // The store closes the editor on every success (and keeps it, frozen
        // or editable, on anything else): nothing to close from here, and a
        // close from here could land on a newer sheet.
        Task { await store.saveEvent(draft, request, via: control) }
    }

    /// The guest terms the save will use, so the problem line judges the
    /// draft exactly as `saveEvent` will.
    private func terms(_ draft: EventDraft) -> GuestTerms {
        store.guestPlan(draft, request).terms(owner: store.calendarAccountEmail)
    }

    /// "add event" on the Mac's footer, "Add event" as a button here.
    private func sentence(_ label: String) -> String {
        label.prefix(1).uppercased() + label.dropFirst()
    }

    private func problem(_ draft: EventDraft, now: Date) -> EventDraftProblem? {
        switch request.mode {
        case .create:
            return EventForm.problem(draft, today: now, calendar: cal, guests: terms(draft))
        case .edit:
            if case .failure(let p) = EventForm.patch(
                draft, from: request.initial, today: now, calendar: cal, guests: terms(draft))
            {
                return p
            }
            return nil
        }
    }

    /// Google may have made the change and the page has not shown it: the
    /// fields freeze, and only the Mac pane's frozen controls are on offer
    /// (`EventPaneState.frozenControls`), with its words for them.
    private func frozen(_ state: EventPaneState) -> some View {
        let controls = state.frozenControls
        return Section {
            Text(CalendarWrite.uncertainUnseenText)
                .font(Typo.ui(14))
                .foregroundStyle(Palette.inkDim)
            if controls.contains(.checkAgain) {
                Button("Check again") {
                    Task { await store.checkEventAgain(request) }
                }
                .disabled(store.calendarWriting)
            }
            if controls.contains(.sendAgain), let sent = store.calendarPaneSent {
                Button(sentence(EventPaneState.sendAgainLabel(sent))) {
                    Task { await store.sendEventAgain(request) }
                }
                .disabled(store.calendarWriting)
            }
            if controls.contains(.close) {
                Button("Close", role: .cancel) { store.closeEventEditor() }
            }
        }
    }

    // MARK: guests

    /// The guests line, as the plan has it: a field on your own calendar,
    /// the reason it is closed elsewhere, nothing on a daemon too old to
    /// invite.
    @ViewBuilder
    private func guests(_ draft: EventDraft, plan: GuestPlan) -> some View {
        switch plan.access {
        case .field:
            Section {
                TextField("Add guests by email", text: field(\.guests), axis: .vertical)
                    .keyboardType(.emailAddress)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
            } footer: {
                // The email keyboard has no comma on its first layer: say how
                // addresses are separated, or a space reads as one bad address.
                Text(
                    plan.after == 0
                        ? "Separate addresses with commas or new lines."
                        : "Separate addresses with commas or new lines. Google emails "
                            + (plan.after == 1 ? "your guest" : "your \(plan.after) guests")
                            + " only when you press the button that says so.")
            }
        case .closed(let bar):
            Section {
                // Who is on it, read-only: whoever "Save and tell guests"
                // would email.
                if let event = request.event, let names = EventGuests.names(event) {
                    Text(names)
                        .font(Typo.ui(14))
                        .foregroundStyle(Palette.ink)
                }
                Text(bar.text)
                    .font(Typo.ui(13))
                    .foregroundStyle(Palette.inkFaint)
                if plan.silent {
                    Text(EventGuests.silentLine)
                        .font(Typo.ui(13))
                        .foregroundStyle(Palette.inkFaint)
                }
            }
        case .none:
            EmptyView()
        }
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

    /// The Mac pane's rule (`setAllDay`): turning all day off fills in the
    /// hours an all-day event never had and lets the end day follow the start
    /// again, so a three-day run does not become a three-day meeting. The
    /// title is read again either way.
    private func allDayBinding(now: Date) -> Binding<Bool> {
        Binding(
            get: { (store.calendarDraft ?? request.initial).allDay },
            set: { on in
                update { d in
                    d.allDay = on
                    if !on {
                        if EventForm.trimmed(d.start).isEmpty { d.start = "9am" }
                        if EventForm.trimmed(d.end).isEmpty { d.end = "10am" }
                        d.endDate = ""
                    }
                    d = EventForm.refilled(d, today: now, calendar: cal)
                }
            })
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
                // From the wall clock, not minutes after midnight: on a
                // daylight-saving day those are an hour apart.
                EventForm.at(base, minutes(draft, end: end) % 1440, cal) ?? base
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
