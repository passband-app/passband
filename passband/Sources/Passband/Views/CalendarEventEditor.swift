// THE EVENT EDITOR: a small card for making an event on one of the user's own
// Google calendars, or changing one. The first thing in Passband that writes
// to a real calendar, so it is plain about what it will do: the card says,
// under the times, exactly what it will save, and a delete asks once.
//
// SMALL ON PURPOSE. Title, calendar, all day, the day and the two times,
// location, notes. No attendees, no reminders, no repeat, no colour: those
// are Google Calendar's, one ↵ away from the detail pane, and a card with
// fifteen fields is a form, not a quick edit.
//
// TYPED, NOT PICKED (Lib/EventDraft.swift has the grammar). The date and the
// times are text wells like every other field in the app's cards, read the
// way people type them ("fri", "Oct 8", "7:45pm", "1930"); the line under
// them says what they resolved to. A stepper date picker would bring AppKit's
// bezel into a glass card, and a graphical one is a second calendar on a
// calendar page.
//
// Built like the group editor and the connect card: an `OverlayScrim` and a
// `ModalCard` in the action layer, its own modal key context, Escape and
// Enter. Enter saves from any field; Escape cancels. A refusal from the
// daemon is one quiet toast and the card stays up with everything typed in it.

#if os(macOS)
    import SwiftUI

    struct CalendarEventEditor: View {
        @Environment(AppStore.self) private var store
        let request: CalendarEditorRequest

        @State private var draft: EventDraft
        /// The footer is asking "Delete this event from Google Calendar?".
        @State private var confirmingDelete = false
        @FocusState private var focus: Field?

        private enum Field: Hashable {
            case title, date, endDate, start, end, location, notes
        }

        init(request: CalendarEditorRequest) {
            self.request = request
            _draft = State(initialValue: request.initial)
        }

        private var editing: TimelineEvent? { request.event }

        var body: some View {
            // "Today" for a typed "fri" or "tomorrow", read once a render.
            let now = Date()
            let cal = Calendar.current
            let problem = blocking(now: now, cal: cal)
            OverlayScrim(onDismiss: cancel) {
                ModalCard(width: 440) {
                    header
                    titleField
                    calendarRow
                    whenFields
                    whenLine(now: now, cal: cal)
                    locationField
                    notesField
                    if confirmingDelete {
                        deleteQuestion
                    } else {
                        footer(problem: problem)
                    }
                }
            }
            .keyContext(.modal)
            .keyBindings(.modal, [
                KeyBinding("Escape", "cancel", allowInInput: true) { escape() },
                KeyBinding("Enter", "save", allowInInput: true) { enter(problem: problem) },
            ])
            .onAppear { focus = .title }
        }

        // MARK: header and title

        private var header: some View {
            HStack(alignment: .firstTextBaseline) {
                Text(editing == nil ? "New event" : "Edit event")
                    .font(Typo.ui(14, weight: .semibold))
                    .foregroundStyle(Palette.ink)
                Spacer(minLength: Space.s8)
                Text("Google Calendar")
                    .font(Typo.micro)
                    .foregroundStyle(Palette.inkFaintest)
            }
        }

        private var titleField: some View {
            TextField("Title", text: $draft.title)
                .textFieldStyle(.plain)
                .font(Typo.ui(15, weight: .semibold))
                .focused($focus, equals: .title)
                .fieldWell()
        }

        // MARK: calendar and all day

        private var calendarRow: some View {
            HStack(alignment: .center, spacing: Space.s10) {
                calendarPicker
                Spacer(minLength: Space.s8)
                Toggle("All day", isOn: allDay)
                    .toggleStyle(.switch)
                    .controlSize(.mini)
                    .tint(Palette.accent)
                    .font(Typo.rowSub)
                    .foregroundStyle(Palette.inkDim)
            }
        }

        /// Which calendar. An edit stays on its own (moving an event between
        /// calendars is Google's `move`, a later wave), so it is a label.
        /// One writable calendar is a label too. A few are chips, each in its
        /// colour, picked directly; past four, a menu of names.
        @ViewBuilder
        private var calendarPicker: some View {
            let list = store.calendarWritable
            if let event = editing {
                CalendarTag(
                    name: event.calendar_name ?? "Your calendar",
                    hue: CalendarColor.parse(event.color).map(ChipHue.calendar) ?? .plain, selected: true)
            } else if list.count <= 1 {
                if let only = list.first {
                    CalendarTag(name: CalendarWrite.name(only), hue: hue(only), selected: true)
                }
            } else if list.count <= 4 {
                HStack(spacing: Space.s6) {
                    ForEach(list) { c in
                        Button {
                            draft.calendarId = c.id
                        } label: {
                            CalendarTag(name: CalendarWrite.name(c), hue: hue(c), selected: draft.calendarId == c.id)
                        }
                        .buttonStyle(.plain)
                        .pointingHand()
                    }
                }
            } else {
                Menu {
                    ForEach(list) { c in
                        Button(CalendarWrite.name(c)) { draft.calendarId = c.id }
                    }
                } label: {
                    Text(list.first { $0.id == draft.calendarId }.map(CalendarWrite.name) ?? "Pick a calendar")
                        .font(Typo.rowSub)
                }
                .menuStyle(.borderlessButton)
                .fixedSize()
            }
        }

        private func hue(_ c: TimelineCalendar) -> ChipHue {
            CalendarColor.parse(c.color).map(ChipHue.calendar) ?? .plain
        }

        /// Turning all day off fills in the hours an all-day event never had
        /// (9 to 10, as a new event's), and drops an end day the timed form
        /// does not show, so nothing hidden rides along into the save.
        private var allDay: Binding<Bool> {
            Binding(
                get: { draft.allDay },
                set: { on in
                    draft.allDay = on
                    guard !on else { return }
                    if EventForm.trimmed(draft.start).isEmpty { draft.start = "9am" }
                    if EventForm.trimmed(draft.end).isEmpty { draft.end = "10am" }
                    if !showsTimedEndDate { draft.endDate = "" }
                })
        }

        // MARK: when

        /// A timed event that runs past the next morning came with its end
        /// day said; the form keeps that well for it, and only for it.
        private var showsTimedEndDate: Bool {
            !request.initial.allDay && !EventForm.trimmed(request.initial.endDate).isEmpty
        }

        @ViewBuilder
        private var whenFields: some View {
            if draft.allDay {
                HStack(spacing: Space.s8) {
                    well("Day", $draft.date, .date)
                    Text("through")
                        .font(Typo.rowSub)
                        .foregroundStyle(Palette.inkFaint)
                        .fixedSize()
                    well("Same day", $draft.endDate, .endDate)
                }
            } else {
                HStack(spacing: Space.s8) {
                    well("Day", $draft.date, .date)
                    well("Start", $draft.start, .start)
                        .frame(width: Typo.measure(84))
                    Text("to")
                        .font(Typo.rowSub)
                        .foregroundStyle(Palette.inkFaint)
                        .fixedSize()
                    well("End", $draft.end, .end)
                        .frame(width: Typo.measure(84))
                }
                if showsTimedEndDate {
                    HStack(spacing: Space.s8) {
                        Text("ends on")
                            .font(Typo.rowSub)
                            .foregroundStyle(Palette.inkFaint)
                            .fixedSize()
                        well("End day", $draft.endDate, .endDate)
                    }
                }
            }
        }

        private func well(_ placeholder: String, _ text: Binding<String>, _ field: Field) -> some View {
            TextField(placeholder, text: text)
                .textFieldStyle(.plain)
                .monospacedDigit()
                .focused($focus, equals: field)
                .fieldWell()
        }

        /// What the card will save, said back: "Thu Oct 8, 7:45pm to 8:45pm".
        /// Or, while the times do not read, what to type instead, in the
        /// warning ink: a hint, not an error.
        private func whenLine(now: Date, cal: Calendar) -> some View {
            let text: String
            let tone: Color
            switch EventForm.when(draft, today: now, calendar: cal) {
            case .success(let w):
                (text, tone) = (EventForm.summary(w, calendar: cal), Palette.inkDim)
            case .failure(let p):
                (text, tone) = (p.text, Palette.warn)
            }
            return Text(text)
                .font(Typo.micro)
                .foregroundStyle(tone)
                .lineLimit(1)
                .padding(.leading, Space.s2)
                .padding(.top, -Space.s4)
        }

        // MARK: location and notes

        private var locationField: some View {
            TextField("Location", text: $draft.location)
                .textFieldStyle(.plain)
                .focused($focus, equals: .location)
                .fieldWell()
        }

        private var notesField: some View {
            TextField("Notes", text: $draft.notes, axis: .vertical)
                .textFieldStyle(.plain)
                .lineLimit(2...6)
                .focused($focus, equals: .notes)
                .fieldWell()
        }

        // MARK: footer

        private func footer(problem: EventDraftProblem?) -> some View {
            // The time's problems are said on the line under the times; the
            // footer says the rest (a title, a calendar, notes too long).
            let note = problem.flatMap { Self.timeProblems.contains($0) ? nil : $0.text }
            return VStack(alignment: .leading, spacing: Space.scaled(9)) {
                HStack(spacing: Space.s8) {
                    if editing != nil {
                        Button("Delete event") { confirmingDelete = true }
                            .buttonStyle(.textAction)
                            .font(Typo.rowSub)
                            .pointingHand()
                            .disabled(store.calendarWriting)
                    }
                    if let note {
                        Text(note)
                            .font(Typo.micro)
                            .foregroundStyle(Palette.warn)
                            .lineLimit(1)
                    }
                    Spacer(minLength: Space.s8)
                    Button("Cancel", action: cancel)
                        .buttonStyle(.glass)
                    Button(saveLabel) { enter(problem: problem) }
                        .buttonStyle(.glassProminent)
                        .tint(Palette.accent)
                        .disabled(problem != nil || store.calendarWriting)
                }
                HStack(spacing: Space.s4) {
                    KeyHint("↵", editing == nil ? "add" : "save")
                    Text("·").font(Typo.micro).foregroundStyle(Palette.inkFaintest)
                    KeyHint("esc", "cancel")
                    Spacer()
                }
            }
            .padding(.top, Space.s4)
        }

        private var saveLabel: String {
            if store.calendarWriting { return "Saving…" }
            return editing == nil ? "Add event" : "Save"
        }

        /// The delete question, in the card's own footer: asked once, the
        /// fields still there above it, Escape back to them.
        private var deleteQuestion: some View {
            VStack(alignment: .leading, spacing: Space.s8) {
                DeleteQuestion()
                HStack(spacing: Space.s8) {
                    KeyHint("↵", "delete")
                    Text("·").font(Typo.micro).foregroundStyle(Palette.inkFaintest)
                    KeyHint("esc", "keep it")
                    Spacer(minLength: Space.s8)
                    Button("Keep it") { confirmingDelete = false }
                        .buttonStyle(.glass)
                    Button(store.calendarWriting ? "Deleting…" : "Delete event") { deleteNow() }
                        .buttonStyle(.glassProminent)
                        .tint(Palette.danger)
                        .disabled(store.calendarWriting)
                }
            }
            .padding(.top, Space.s4)
        }

        // MARK: verbs

        private static let timeProblems: Set<EventDraftProblem> = [
            .badDate, .badEndDate, .badStart, .badEnd, .noLength, .endsBeforeStart,
        ]

        /// The first thing in the way of saving, or nil.
        private func blocking(now: Date, cal: Calendar) -> EventDraftProblem? {
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

        private func enter(problem: EventDraftProblem?) {
            if confirmingDelete {
                deleteNow()
                return
            }
            guard problem == nil, !store.calendarWriting else { return }
            let draft = draft
            Task { await store.saveEvent(draft, request) }
        }

        private func escape() {
            if confirmingDelete {
                confirmingDelete = false
            } else {
                cancel()
            }
        }

        private func cancel() {
            store.closeEventEditor()
        }

        private func deleteNow() {
            guard let event = editing, !store.calendarWriting else { return }
            Task { await store.deleteEvent(event) }
        }
    }

    // MARK: - the delete question, on its own

    /// ⌫ or the detail's "delete" on an event: the same question as the
    /// editor's footer, on a card of its own. Asked once; Enter deletes,
    /// Escape keeps it.
    struct CalendarDeleteConfirm: View {
        @Environment(AppStore.self) private var store
        let ask: CalendarDeleteAsk

        var body: some View {
            OverlayScrim(onDismiss: { store.cancelDeleteEvent() }) {
                ModalCard(width: 380) {
                    DeleteQuestion()
                    EventStub(event: ask.event)
                    HStack(spacing: Space.s8) {
                        Spacer(minLength: 0)
                        Button("Keep it") { store.cancelDeleteEvent() }
                            .buttonStyle(.glass)
                        Button(store.calendarWriting ? "Deleting…" : "Delete event") { deleteNow() }
                            .buttonStyle(.glassProminent)
                            .tint(Palette.danger)
                            .disabled(store.calendarWriting)
                    }
                }
            }
            .keyContext(.modal)
            .keyBindings(.modal, [
                KeyBinding("Escape", "keep it", allowInInput: true) { store.cancelDeleteEvent() },
                KeyBinding("Enter", "delete", allowInInput: true) { deleteNow() },
            ])
        }

        private func deleteNow() {
            guard !store.calendarWriting else { return }
            let event = ask.event
            Task { await store.deleteEvent(event) }
        }
    }

    /// The question both delete paths ask, in the same words.
    private struct DeleteQuestion: View {
        var body: some View {
            VStack(alignment: .leading, spacing: Space.s4) {
                Text("Delete this event from Google Calendar?")
                    .font(Typo.ui(13, weight: .semibold))
                    .foregroundStyle(Palette.ink)
                    .fixedSize(horizontal: false, vertical: true)
                Text("It goes from your calendar everywhere, and Passband cannot put it back.")
                    .font(Typo.micro)
                    .foregroundStyle(Palette.inkFaint)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
    }

    /// The event being asked about, on a neutral well: its colour, its title
    /// and when, so the question is never about the wrong one.
    private struct EventStub: View {
        let event: TimelineEvent

        var body: some View {
            let cal = Calendar.current
            let item = Agenda.item(event, calendar: cal)
            let inks = ChipColors.inks(CalendarColor.parse(event.color).map(ChipHue.calendar) ?? .plain)
            HStack(alignment: .firstTextBaseline, spacing: Space.s8) {
                RoundedRectangle(cornerRadius: 3, style: .continuous)
                    .fill(inks.edge)
                    .frame(width: 9, height: 9)
                VStack(alignment: .leading, spacing: Space.s2) {
                    // The calendar's own text: plain Text, never markup.
                    Text(verbatim: item?.title ?? event.title ?? "Untitled event")
                        .font(Typo.ui(12.5, weight: .semibold))
                        .foregroundStyle(Palette.ink)
                        .lineLimit(2)
                    if let item {
                        let full = Agenda.fullTime(item, calendar: cal)
                        Text(full.end.map { "\(full.lead) to \($0)" } ?? full.lead)
                            .font(Typo.micro)
                            .foregroundStyle(Palette.inkDim)
                    }
                }
                Spacer(minLength: 0)
            }
            .padding(.horizontal, Space.s10)
            .padding(.vertical, Space.scaled(7))
            .background(Palette.hairline.opacity(0.6), in: RoundedRectangle(cornerRadius: 7, style: .continuous))
        }
    }

    /// A calendar named in its colour: a swatch and the name, on a capsule
    /// that fills with the calendar's tint and takes an even edge when it is
    /// the one picked.
    private struct CalendarTag: View {
        let name: String
        let hue: ChipHue
        let selected: Bool

        var body: some View {
            let inks = ChipColors.inks(hue)
            HStack(spacing: Space.s6) {
                Circle()
                    .fill(inks.edge)
                    .frame(width: 7, height: 7)
                Text(verbatim: name)
                    .font(Typo.ui(11.5, weight: .medium))
                    .foregroundStyle(selected ? Palette.ink : Palette.inkDim)
                    .lineLimit(1)
                    .frame(maxWidth: Typo.measure(130), alignment: .leading)
            }
            .padding(.horizontal, Space.s8)
            .padding(.vertical, Space.scaled(4))
            .background(Capsule().fill(inks.fill.opacity(selected ? 1 : 0)))
            .overlay(
                Capsule().strokeBorder(
                    selected ? inks.edge.opacity(0.8) : Palette.hairlineStrong, lineWidth: 1))
            .contentShape(Capsule())
        }
    }
#endif
