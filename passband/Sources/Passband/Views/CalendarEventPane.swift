// THE EVENT PANE: making an event on one of the user's own Google calendars,
// or changing one, in a pane that takes half the window the way the composer
// does (wave 4, the founder: "The new event button should work like the
// compose pane ... all of the new event selection should be in there,
// including selecting which calendar it goes on").
//
// BUILT ON THE COMPOSER'S BONES (Views/ComposePane.swift), on purpose: the
// same strip of the window, the same glass edge and leftward shadow, the same
// header voice, one card with ruled lines for the fields and the free text
// under them, the verb in the footer with its key beside it. No scrim and no
// blur: the month beside it shrinks and stays live, a day clicked there sets
// this draft's date, and the draft shows on the grid as a dashed chip in its
// calendar's colour. Esc closes the pane like every other panel.
//
// A NEW HOST FOR THE SAME LOGIC. The fields are Lib/EventDraft.swift's, typed
// and not picked ("fri", "Oct 8", "7:45pm", "1930"), with the line under them
// saying exactly what will be saved. The draft itself lives in the store
// (`AppStore.calendarDraft`), which is how the page can draw it.
//
// SMALL ON PURPOSE. Title, calendar, when, guests, where, notes. No
// reminders, no repeat: those are Google Calendar's, one ↵ away from the
// detail pane.
//
// GUESTS (wave 5, Lib/EventGuests.swift). The guests line is the composer's
// own recipient field. An invitation, an update and a cancellation are Google
// Calendar's mail, sent by Google, and cannot be unsent, so the pane holds
// three rules: the control that saves says what it will do ("add and invite
// 3", "save and tell guests") and is the only thing that tells anyone; with a
// guest on either side of a save, plain ↵ saves nothing; and a guest list
// that is not the user's to change, or that the daemon holds only part of,
// is shown and never sent back.
//
// WHERE (Lib/PlaceLookup.swift): the location line suggests places as it is
// typed, from Apple's MapKit. The typed text goes to Apple and nothing else
// does; free text is always allowed and is replaced only by a pick.
//
// KEYS. ⌘↵ presses the footer's main control from anywhere in the pane. ↵
// saves from a single-line field ONLY when the save tells nobody; it also
// picks a highlighted place or contact, makes a typed address a guest, and is
// a new line in the notes. With no field focused it is the month's. Esc
// closes the place list, then the delete question, then the pane.

#if os(macOS)
    import SwiftUI

    struct CalendarEventPane: View {
        @Environment(AppStore.self) private var store
        let request: CalendarEditorRequest

        /// The footer is asking "Delete this event from Google Calendar?".
        @State private var confirmingDelete = false
        /// The location line's lookup. Made when the pane appears, so a
        /// re-render of the shell never builds a second MapKit completer.
        @State private var places: PlaceLookup?
        /// What the daemon knows of each guest typed so far, by address
        /// (case-folded): a name to call them, and their zone when it has
        /// one. Filled in behind the pane; nothing waits on it.
        @State private var guestPlaces: [String: GuestPlace] = [:]
        @FocusState private var focus: Field?

        private enum Field: Hashable {
            case title, date, endDate, start, end, guests, location, notes
        }

        private struct GuestPlace: Equatable {
            let name: String
            let zone: TimeZone?
        }

        private var draft: EventDraft { store.calendarDraft ?? request.initial }
        private var editing: TimelineEvent? { request.event }

        var body: some View {
            // "Today" for a typed "fri" or "tomorrow", read once a render.
            let now = Date()
            let cal = Calendar.current
            let plan = store.guestPlan(draft, request)
            let problem = blocking(plan: plan, now: now, cal: cal)
            VStack(alignment: .leading, spacing: 0) {
                header
                card(plan: plan, problem: problem, now: now, cal: cal)
                    .padding(.horizontal, Space.scaled(18))
                    .padding(.vertical, Space.s14)
                    .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
                footer(problem: problem, plan: plan)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            // The composer's own edge: glass, and a shadow thrown left over
            // the page it stands beside.
            .passbandGlass(.pane, cornerRadius: 0, tint: Palette.glassTintStrong)
            .shadow(color: .black.opacity(0.24), radius: 40, x: -14)
            .keyContext(.modal)
            .keyBindings(.modal, bindings(problem: problem, plan: plan))
            // Where each guest is, asked of the daemon as they are added.
            .task(id: guestKeys(plan)) { await lookUpGuests(plan) }
            .onAppear {
                let lookup = PlaceLookup(provider: MapKitPlaces())
                lookup.write = { text in patch { $0.location = text } }
                places = lookup
                focus = .title
            }
            .onDisappear { places?.stop() }
            // A click elsewhere defocuses the line; a list about text the
            // caret has left must not linger.
            .onChange(of: focus) { _, now in
                if now != .location { places?.dismiss() }
            }
        }

        // MARK: header

        private var header: some View {
            HStack(alignment: .firstTextBaseline, spacing: Space.s8) {
                Text(editing == nil ? "new event" : "edit event")
                    .font(Typo.zoneTitle)
                    .foregroundStyle(Palette.ink)
                // Where it is going, in the spot the composer says how many
                // drafts wait: a fact about the pane, not a control.
                Text("Google Calendar")
                    .font(Typo.micro)
                    .foregroundStyle(Palette.inkFaintest)
                Spacer()
            }
            .padding(.horizontal, Space.scaled(18))
            .padding(.vertical, Space.scaled(13))
            .overlay(alignment: .bottom) { Hairline() }
        }

        // MARK: the card

        /// ONE CARD, as the composer's is: the title, then the labelled lines
        /// ruled apart, then the notes on the same ground taking what is left.
        private func card(plan: GuestPlan, problem: EventDraftProblem?, now: Date, cal: Calendar) -> some View {
            VStack(alignment: .leading, spacing: 0) {
                TextField("Title", text: bind(\.title))
                    .textFieldStyle(.plain)
                    .font(Typo.ui(15, weight: .semibold))
                    .foregroundStyle(Palette.ink)
                    .focused($focus, equals: .title)
                    .padding(.horizontal, InlineFieldMetrics.inset)
                    .padding(.vertical, Space.s10)
                Hairline()
                InlineFieldRow(label: "calendar") { calendarPicker }
                Hairline()
                InlineFieldRow(label: "when") { whenFields(now: now, cal: cal) }
                guestsSection(plan: plan, problem: problem, now: now, cal: cal)
                Hairline()
                InlineFieldRow(label: "where") {
                    TextField("Add a place", text: bind(\.location))
                        .textFieldStyle(.plain)
                        .focused($focus, equals: .location)
                        .onChange(of: draft.location) { _, text in places?.textChanged(text) }
                }
                if let places, places.picker.isOpen {
                    PlaceSuggestions(picker: places.picker) { places.pick($0) }
                        .modifier(InlineFieldTuck(active: true))
                }
                Hairline()
                notes
            }
            .frame(maxHeight: .infinity, alignment: .top)
            // Near-opaque, like every input well: a translucent card over a
            // busy wallpaper leaves typed text unreadable.
            .background(
                RoundedRectangle(cornerRadius: 10, style: .continuous)
                    .fill(Palette.canvas.opacity(0.65))
            )
            .overlay(
                RoundedRectangle(cornerRadius: 10, style: .continuous)
                    .strokeBorder(Palette.hairlineStrong, lineWidth: 0.75))
        }

        // MARK: calendar

        /// Which calendar it goes on: every calendar the user can write to,
        /// each a tag in its own colour, the primary one first and picked.
        /// Picking another recolours the draft's chip on the grid. An edit
        /// stays on its own calendar (moving an event between calendars is
        /// Google's `move`, a later wave), so it is a label. Past a handful,
        /// a menu of names.
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
            } else if list.count <= 8 {
                FlowLayout(spacing: Space.s6) {
                    ForEach(list) { c in
                        Button {
                            patch { $0.calendarId = c.id }
                        } label: {
                            CalendarTag(name: CalendarWrite.name(c), hue: hue(c), selected: draft.calendarId == c.id)
                        }
                        .buttonStyle(.plain)
                        .pointingHand()
                        .accessibilityAddTraits(draft.calendarId == c.id ? .isSelected : [])
                    }
                }
                // The tags sit a little above the label's text line, so the
                // first row of them centres on it.
                .padding(.vertical, -Space.scaled(2))
            } else {
                Menu {
                    ForEach(list) { c in
                        Button(CalendarWrite.name(c)) { patch { $0.calendarId = c.id } }
                    }
                } label: {
                    if let picked = list.first(where: { $0.id == draft.calendarId }) {
                        CalendarTag(name: CalendarWrite.name(picked), hue: hue(picked), selected: true)
                    } else {
                        Text("Pick a calendar").font(Typo.rowSub)
                    }
                }
                .menuStyle(.borderlessButton)
                .fixedSize()
            }
        }

        private func hue(_ c: TimelineCalendar) -> ChipHue {
            CalendarColor.parse(c.color).map(ChipHue.calendar) ?? .plain
        }

        // MARK: when

        /// A timed event that runs past the next morning came with its end
        /// day said; the form keeps that well for it, and only for it.
        private var showsTimedEndDate: Bool {
            !request.initial.allDay && !EventForm.trimmed(request.initial.endDate).isEmpty
        }

        /// The day and the two times as typed wells, the all-day switch, and
        /// under them what they resolve to. One line where the pane is wide
        /// enough for it, the day above the times where it is not.
        private func whenFields(now: Date, cal: Calendar) -> some View {
            VStack(alignment: .leading, spacing: Space.s6) {
                ViewThatFits(in: .horizontal) {
                    HStack(spacing: Space.s6) {
                        dayWell
                        span
                        Spacer(minLength: Space.s8)
                        allDayToggle
                    }
                    VStack(alignment: .leading, spacing: Space.s6) {
                        HStack(spacing: Space.s6) {
                            dayWell
                            Spacer(minLength: Space.s8)
                            allDayToggle
                        }
                        HStack(spacing: Space.s6) { span }
                    }
                }
                if !draft.allDay, showsTimedEndDate {
                    HStack(spacing: Space.s6) {
                        joiner("ends on")
                        well("End day", \.endDate, .endDate, width: Typo.measure(112))
                    }
                }
                whenLine(now: now, cal: cal)
            }
            // The wells are taller than a line of text; pulled up so the
            // first of them centres on the label beside it.
            .padding(.vertical, -Space.scaled(3))
        }

        private var dayWell: some View {
            well("Day", \.date, .date, width: Typo.measure(112))
        }

        @ViewBuilder
        private var span: some View {
            if draft.allDay {
                joiner("through")
                well("Same day", \.endDate, .endDate, width: Typo.measure(112))
            } else {
                well("Start", \.start, .start, width: Typo.measure(70))
                joiner("to")
                well("End", \.end, .end, width: Typo.measure(70))
            }
        }

        private func joiner(_ word: String) -> some View {
            Text(word)
                .font(Typo.rowSub)
                .foregroundStyle(Palette.inkFaint)
                .fixedSize()
        }

        /// A typed date or time: a small well of its own on the card, so the
        /// three of them read as three things to click into, and the line
        /// stays one line.
        private func well(
            _ placeholder: String, _ keyPath: WritableKeyPath<EventDraft, String>, _ field: Field, width: CGFloat
        ) -> some View {
            let shape = RoundedRectangle(cornerRadius: 6, style: .continuous)
            return TextField(placeholder, text: bind(keyPath))
                .textFieldStyle(.plain)
                .font(Typo.ui(13))
                .monospacedDigit()
                .foregroundStyle(Palette.ink)
                .focused($focus, equals: field)
                .padding(.horizontal, Space.scaled(7))
                .padding(.vertical, Space.scaled(3))
                .frame(width: width)
                .background(shape.fill(Palette.hairline.opacity(0.5)))
                // The well being typed in takes an even edge, all the way round.
                .overlay(shape.strokeBorder(Palette.accent.opacity(focus == field ? 0.55 : 0), lineWidth: 1))
        }

        /// All day, as the composer draws its own switch: a chip that fills
        /// when it is on.
        private var allDayToggle: some View {
            Button {
                setAllDay(!draft.allDay)
            } label: {
                Chip(
                    text: "all day",
                    tone: draft.allDay ? Palette.accent : Palette.inkFaint,
                    symbol: draft.allDay ? "checkmark" : nil,
                    filled: draft.allDay)
            }
            .buttonStyle(.plain)
            .pointingHand()
            .accessibilityLabel("All day")
            .accessibilityAddTraits(draft.allDay ? .isSelected : [])
        }

        /// Turning all day off fills in the hours an all-day event never had
        /// (9 to 10, as a new event's), and drops an end day the timed form
        /// does not show, so nothing hidden rides along into the save.
        private func setAllDay(_ on: Bool) {
            patch { d in
                d.allDay = on
                guard !on else { return }
                if EventForm.trimmed(d.start).isEmpty { d.start = "9am" }
                if EventForm.trimmed(d.end).isEmpty { d.end = "10am" }
                if !showsTimedEndDate { d.endDate = "" }
            }
        }

        /// What the pane will save, said back: "Thu Oct 8, 7:45pm to 8:45pm".
        /// Or, while the times do not read, what to type instead, in the
        /// warning ink: a hint, not an error.
        private func whenLine(now: Date, cal: Calendar) -> some View {
            let text: String
            let tone: Color
            switch EventForm.when(draft, today: now, calendar: cal) {
            case .success(let w):
                (text, tone) = (EventForm.summary(w, calendar: cal), Palette.inkFaint)
            case .failure(let p):
                (text, tone) = (p.text, Palette.warn)
            }
            return Text(text)
                .font(Typo.micro)
                .foregroundStyle(tone)
                .lineLimit(1)
                .padding(.leading, Space.s2)
        }

        // MARK: guests

        /// THE GUESTS LINE, between when and where: the composer's own
        /// recipient field (pills, contacts as you type, several addresses
        /// pasted at once, backspace to take one off), and under it what is
        /// true about the people on it. Closed, it lists who is on the event
        /// and says in one line why they cannot be changed here. On a daemon
        /// older than guests there is no line at all.
        @ViewBuilder
        private func guestsSection(
            plan: GuestPlan, problem: EventDraftProblem?, now: Date, cal: Calendar
        ) -> some View {
            switch plan.access {
            case .none:
                EmptyView()
            case .field:
                Hairline()
                RecipientField(
                    text: bind(\.guests), focus: $focus, field: Field.guests, label: "guests",
                    placeholder: "Add people by email", inline: true)
                guestNotes(plan: plan, problem: problem, now: now, cal: cal)
            case .closed(let bar):
                Hairline()
                InlineFieldRow(label: "guests") {
                    VStack(alignment: .leading, spacing: Space.s4) {
                        if let names = editing.flatMap({ EventGuests.names($0) }) {
                            // Google's text about other people: plain Text.
                            Text(verbatim: names)
                                .lineLimit(2)
                        }
                        // A calendar the guests cannot be told from says
                        // that instead: the one fact the footer's single
                        // "save without telling" needs explained.
                        Text(plan.silent && bar == .otherCalendar ? EventGuests.silentLine : bar.text)
                            .font(Typo.micro)
                            .foregroundStyle(Palette.inkFaint)
                            .fixedSize(horizontal: false, vertical: true)
                        if plan.silent, bar != .otherCalendar {
                            Text(EventGuests.silentLine)
                                .font(Typo.micro)
                                .foregroundStyle(Palette.inkFaint)
                                .fixedSize(horizontal: false, vertical: true)
                        }
                    }
                }
            }
        }

        /// Under the field, in the value column: what is wrong with the line
        /// (in the warning ink, as the times say theirs), when the event is
        /// for each guest whose zone the daemon knows, and the one sentence
        /// about email. Nothing at all with no guests typed.
        @ViewBuilder
        private func guestNotes(
            plan: GuestPlan, problem: EventDraftProblem?, now: Date, cal: Calendar
        ) -> some View {
            let wrong = problem.flatMap { Self.guestProblems.contains($0) ? $0.text : nil }
            let times = guestTimes(now: now, cal: cal)
            let email = EventGuests.emailLine(
                editing: editing != nil, guests: plan.after, account: store.calendarAccountEmail)
            if wrong != nil || times != nil || email != nil {
                VStack(alignment: .leading, spacing: Space.s2) {
                    if let wrong {
                        Text(wrong).foregroundStyle(Palette.warn)
                    }
                    if let times {
                        // Names are email-derived: plain Text, never markup.
                        Text(verbatim: times).foregroundStyle(Palette.inkDim)
                    }
                    if let email {
                        Text(verbatim: email).foregroundStyle(Palette.inkFaint)
                    }
                }
                .font(Typo.micro)
                .fixedSize(horizontal: false, vertical: true)
                .frame(maxWidth: .infinity, alignment: .leading)
                // The field's own line ends in its padding; these belong to
                // it, so they sit up under the pills.
                .padding(.top, -Space.s4)
                .modifier(InlineFieldTuck(active: true))
            }
        }

        /// "10am ET for Sam · 4pm GMT+1 for Ana": the start, where each
        /// guest is. Only guests whose zone the daemon knows, only when
        /// their clock differs from the user's, and never for an all-day
        /// event.
        private func guestTimes(now: Date, cal: Calendar) -> String? {
            guard case .success(let when) = EventForm.when(draft, today: now, calendar: cal), !when.allDay
            else { return nil }
            let parts = EventGuests.read(draft.guests).addresses.compactMap { address -> String? in
                guard let place = guestPlaces[EventGuests.key(address)], let zone = place.zone else { return nil }
                return EventGuests.localTime(start: when.start, zone: zone, name: place.name, calendar: cal)
            }
            return parts.isEmpty ? nil : parts.joined(separator: " \u{00B7} ")
        }

        /// The guests typed so far, as the lookup's identity: it runs again
        /// when somebody is added, and not when a title is typed.
        private func guestKeys(_ plan: GuestPlan) -> [String] {
            guard plan.access == .field else { return [] }
            return EventGuests.read(draft.guests).addresses.map(EventGuests.key).sorted()
        }

        /// Ask the daemon's contact card for each guest not asked about yet
        /// (`/client/person` carries an inferred timezone). Behind the pane:
        /// a guest it knows nothing of simply gets no line, and a failure is
        /// tried again the next time the list changes.
        private func lookUpGuests(_ plan: GuestPlan) async {
            let missing = guestKeys(plan).filter { guestPlaces[$0] == nil }
            guard !missing.isEmpty else { return }
            // A beat, so an address still being typed is not looked up at
            // every letter of its domain.
            try? await Task.sleep(for: .milliseconds(350))
            for key in missing {
                guard !Task.isCancelled else { return }
                guard let card = try? await APIClient.shared.person(key) else { continue }
                guestPlaces[key] = GuestPlace(
                    name: EventGuests.shortName(name: card.display_name, email: key),
                    zone: card.timezone?.zone.flatMap { TimeZone(identifier: $0) })
            }
        }

        /// ↵ in the guests line with a whole address typed and no list up:
        /// the address becomes a guest, as a comma would make it. The field
        /// re-reads an outside write, and a list ending in a comma is all
        /// pills.
        private func commitTypedGuest() {
            let text = draft.guests.trimmingCharacters(in: .whitespacesAndNewlines)
            guard let last = EventGuests.split(text).last, !text.hasSuffix(","),
                EventGuests.isAddress(EventGuests.bare(last))
            else { return }
            patch { $0.guests = text + "," }
        }

        // MARK: notes

        /// The free text, on the card's own ground and taking what is left of
        /// it, as the composer's body does. A real text view: ↵ is a new line
        /// here, and ⌘↵ still saves.
        private var notes: some View {
            TextEditor(text: bind(\.notes))
                .font(Typo.ui(13))
                .foregroundStyle(Palette.ink)
                .scrollContentBackground(.hidden)
                .scrollIndicators(.hidden)
                .focused($focus, equals: .notes)
                // The text view pads its own line by five points; the rest
                // puts the notes on the labels' edge.
                .padding(.horizontal, InlineFieldMetrics.inset - 5)
                .padding(.vertical, Space.s8)
                .frame(minHeight: Space.scaled(72), maxHeight: .infinity)
                .overlay(alignment: .topLeading) {
                    if draft.notes.isEmpty {
                        Text("Notes")
                            .font(Typo.ui(13))
                            .foregroundStyle(Palette.inkFaintest)
                            .padding(.horizontal, InlineFieldMetrics.inset)
                            .padding(.vertical, Space.s8)
                            .allowsHitTesting(false)
                    }
                }
        }

        // MARK: footer

        @ViewBuilder
        private func footer(problem: EventDraftProblem?, plan: GuestPlan) -> some View {
            Group {
                if confirmingDelete {
                    deleteQuestion
                } else {
                    verbs(problem: problem, plan: plan)
                }
            }
            .padding(.horizontal, Space.scaled(18))
            .padding(.vertical, Space.s12)
            .overlay(alignment: .top) { Hairline() }
        }

        /// One statement per verb, its key beside it, as in the composer. No
        /// cancel button: Esc closes this pane like every other panel.
        ///
        /// THE CONTROL SAYS WHAT IT DOES (`EventGuests.controls`). "add and
        /// invite 3" invites; an edit to an event with guests offers "save
        /// and tell guests" and, quieter beside it, "save without telling".
        /// Whichever is pressed is handed to the store as itself, and that
        /// is the only way Google is ever asked to email anyone.
        private func verbs(problem: EventDraftProblem?, plan: GuestPlan) -> some View {
            // The time's problems are said on the line under the times, the
            // guests' under the guests, and a missing title by the empty
            // title itself; the footer says the rest (a calendar, a title or
            // notes too long).
            let note = problem.flatMap { Self.saidElsewhere.contains($0) ? nil : $0.text }
            let controls = plan.controls
            let blocked = problem != nil || store.calendarWriting
            let remove = Group {
                if editing != nil {
                    Button("delete event") { confirmingDelete = true }
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
            }
            let saves = Group {
                if let quiet = controls.quiet {
                    Button(quiet.label) { save(quiet, problem: problem) }
                        .buttonStyle(.glass)
                        .disabled(blocked)
                }
                Button { save(controls.primary, problem: problem) } label: {
                    keyed(store.calendarWriting ? "saving…" : controls.primary.label, "⌘↵")
                }
                .buttonStyle(.glassProminent)
                .tint(Palette.accent)
                .disabled(blocked)
            }
            return VStack(alignment: .trailing, spacing: Space.s8) {
                // ENTER DOES NOT SEND, said where the eye is when it matters.
                if !EventGuests.enterMaySave(controls) {
                    Text("With guests, only ⌘↵ or a click saves.")
                        .font(Typo.micro)
                        .foregroundStyle(Palette.inkFaintest)
                }
                // One line where the pane is wide enough for three controls,
                // the saves under the delete where it is not.
                ViewThatFits(in: .horizontal) {
                    HStack(spacing: Space.s8) {
                        remove
                        Spacer(minLength: Space.s8)
                        saves
                    }
                    VStack(alignment: .trailing, spacing: Space.s8) {
                        HStack(spacing: Space.s8) {
                            remove
                            Spacer(minLength: 0)
                        }
                        HStack(spacing: Space.s8) { saves }
                    }
                }
            }
        }

        /// The delete question, in the pane's own footer: asked once, the
        /// fields still there above it, Escape back to them. An event with
        /// guests asks about telling them, and offers both answers by name:
        /// plain ↵ then deletes nothing.
        private var deleteQuestion: some View {
            let wording = deleteWording
            return VStack(alignment: .leading, spacing: Space.s10) {
                DeleteQuestion(wording: wording)
                ViewThatFits(in: .horizontal) {
                    HStack(spacing: Space.s8) {
                        Spacer(minLength: 0)
                        deleteAnswers(wording)
                    }
                    VStack(alignment: .trailing, spacing: Space.s8) { deleteAnswers(wording) }
                        .frame(maxWidth: .infinity, alignment: .trailing)
                }
            }
        }

        @ViewBuilder
        private func deleteAnswers(_ wording: DeleteWording) -> some View {
            Button { confirmingDelete = false } label: { keyed("keep it", "esc") }
                .buttonStyle(.glass)
            if let tell = wording.tell {
                Button(wording.quiet) { deleteNow(tellGuests: false) }
                    .buttonStyle(.glass)
                    .disabled(store.calendarWriting)
                Button { deleteNow(tellGuests: true) } label: {
                    keyed(store.calendarWriting ? "deleting…" : tell, "⌘↵")
                }
                .buttonStyle(.glassProminent)
                .tint(Palette.danger)
                .disabled(store.calendarWriting)
            } else {
                Button { deleteNow(tellGuests: false) } label: {
                    keyed(store.calendarWriting ? "deleting…" : wording.quiet, "↵")
                }
                .buttonStyle(.glassProminent)
                .tint(Palette.danger)
                .disabled(store.calendarWriting)
            }
        }

        /// The question for the event being edited, by its guests.
        private var deleteWording: DeleteWording {
            editing.map { EventGuests.deleteWording($0, feed: store.calendarFeed) }
                ?? EventGuests.deleteWording(guests: 0, truncated: false)
        }

        /// A footer verb with its key beside it, dimmer, in the key chip's
        /// mono: the composer's own.
        private func keyed(_ verb: String, _ key: String) -> some View {
            HStack(spacing: Space.s6) {
                Text(verb)
                Text(key)
                    .font(Typo.mono(10, weight: .medium))
                    .opacity(0.6)
            }
        }

        // MARK: keys

        private func bindings(problem: EventDraftProblem?, plan: GuestPlan) -> [KeyBinding] {
            [
                // The innermost thing open closes first: the place list, the
                // delete question, then the pane.
                KeyBinding("Escape", "close", allowInInput: true) { escape() },
                // ⌘↵ FROM ANYWHERE, the notes included: the footer's main
                // control, by name, exactly as a click on it. Declines under
                // a modal raised over the pane (the ask bar, the shortcuts
                // card), whose field the chord was typed into.
                KeyBinding(declining: "Enter", "save", meta: true, allowInInput: true) {
                    guard !store.modalOverlayOpen else { return false }
                    if confirmingDelete {
                        deleteNow(tellGuests: deleteWording.tell != nil)
                    } else {
                        save(plan.controls.primary, problem: problem)
                    }
                    return true
                },
                // ↵: a highlighted place is picked; a typed address becomes
                // a guest; a single-line field saves. In the notes it is a
                // new line, so it is declined there and the text view has it.
                //
                // ↵ NEVER TELLS ANYONE. With a guest on either side of the
                // save (`EventGuests.enterMaySave`), or guests to tell about
                // a delete, it is swallowed and nothing happens: only ⌘↵ or
                // a click on the control that says so does that.
                KeyBinding(declining: "Enter", "save", allowInInput: true) {
                    guard !store.modalOverlayOpen else { return false }
                    if focus == .location, places?.pick() == true { return true }
                    if confirmingDelete {
                        if deleteWording.tell == nil { deleteNow(tellGuests: false) }
                        return true
                    }
                    if focus == .notes { return false }
                    if focus == .guests {
                        commitTypedGuest()
                        return true
                    }
                    // ONLY FROM A LINE: with no field focused (a day clicked
                    // on the month beside the pane) ↵ is the month's.
                    switch EventGuests.enter(onLine: focus.map(Self.lines.contains) ?? false, plan.controls) {
                    case .decline: return false
                    case .swallow: return true
                    case .save:
                        save(plan.controls.primary, problem: problem)
                        return true
                    }
                },
                // The arrows are the place list's only while it is up and the
                // caret is in its line; otherwise they pass on (the month
                // beside the pane still moves by them).
                KeyBinding(declining: "ArrowDown", "next place", allowInInput: true) {
                    focus == .location && places?.move(1) == true
                },
                KeyBinding(declining: "ArrowUp", "previous place", allowInInput: true) {
                    focus == .location && places?.move(-1) == true
                },
            ]
        }

        private func escape() {
            if places?.dismiss() == true { return }
            if confirmingDelete {
                confirmingDelete = false
                return
            }
            store.closeEventEditor()
        }

        // MARK: verbs

        /// The single-line fields: the ones plain ↵ saves from.
        private static let lines: Set<Field> = [.title, .date, .endDate, .start, .end, .location]

        /// Said under the guests line.
        private static let guestProblems: Set<EventDraftProblem> = [.badGuest, .guestIsYou, .tooManyGuests]

        private static let saidElsewhere: Set<EventDraftProblem> = guestProblems.union([
            .noTitle, .badDate, .badEndDate, .badStart, .badEnd, .noLength, .endsBeforeStart,
        ])

        /// The first thing in the way of saving, or nil.
        private func blocking(plan: GuestPlan, now: Date, cal: Calendar) -> EventDraftProblem? {
            let terms = plan.terms(owner: store.calendarAccountEmail)
            switch request.mode {
            case .create:
                return EventForm.problem(draft, today: now, calendar: cal, guests: terms)
            case .edit:
                if case .failure(let p) = EventForm.patch(
                    draft, from: request.initial, today: now, calendar: cal, guests: terms)
                {
                    return p
                }
                return nil
            }
        }

        /// Save, AS the control that asked: the store decides from the
        /// control itself whether the guests are told.
        private func save(_ control: SaveControl, problem: EventDraftProblem?) {
            guard problem == nil, !store.calendarWriting else { return }
            let draft = draft
            Task { await store.saveEvent(draft, request, via: control) }
        }

        /// `tellGuests` is true from "delete and tell guests" and its ⌘↵,
        /// and from nowhere else.
        private func deleteNow(tellGuests: Bool) {
            guard let event = editing, !store.calendarWriting else { return }
            Task { await store.deleteEvent(event, tellGuests: tellGuests) }
        }

        // MARK: state helpers

        /// Every text field of the pane is bound through here, into the
        /// store's draft: that is what lets the month draw it as it changes.
        private func bind(_ keyPath: WritableKeyPath<EventDraft, String>) -> Binding<String> {
            Binding(
                get: { (store.calendarDraft ?? request.initial)[keyPath: keyPath] },
                set: { value in
                    guard (store.calendarDraft ?? request.initial)[keyPath: keyPath] != value else { return }
                    patch { $0[keyPath: keyPath] = value }
                })
        }

        private func patch(_ mutate: (inout EventDraft) -> Void) {
            // Only while this pane's request is still the one open: a late
            // write (a place looked up after Esc) must not revive a draft.
            guard store.calendarEditor?.id == request.id, var next = store.calendarDraft else { return }
            mutate(&next)
            if next != store.calendarDraft { store.calendarDraft = next }
        }
    }

    // MARK: - the place list

    /// The suggestions under the "where" line, drawn as the recipient lines
    /// draw theirs: a name, what is under it, and the arrowed row on the soft
    /// accent. Place names are MapKit's: plain Text, never markup.
    private struct PlaceSuggestions: View {
        let picker: PlacePicker
        let pick: (Int) -> Void

        var body: some View {
            VStack(alignment: .leading, spacing: 1) {
                ForEach(Array(picker.suggestions.enumerated()), id: \.offset) { i, place in
                    Button {
                        pick(i)
                    } label: {
                        HStack(alignment: .firstTextBaseline, spacing: Space.s8) {
                            Text(verbatim: place.title)
                                .font(Typo.ui(11, weight: .medium))
                                .foregroundStyle(Palette.ink)
                                .lineLimit(1)
                                .layoutPriority(1)
                            Text(verbatim: place.subtitle)
                                .font(Typo.micro)
                                .foregroundStyle(i == picker.highlight ? Palette.inkDim : Palette.inkFaint)
                                .lineLimit(1)
                            Spacer(minLength: 0)
                        }
                        .padding(.horizontal, Space.scaled(9))
                        .padding(.vertical, Space.scaled(5))
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                    .background(
                        RoundedRectangle(cornerRadius: 7, style: .continuous)
                            .fill(i == picker.highlight ? Palette.accentSoft : .clear)
                    )
                    .accessibilityAddTraits(i == picker.highlight ? .isSelected : [])
                }
                // Whose answers these are, and so where the typing went.
                Text("Suggestions from Apple Maps")
                    .font(Typo.micro)
                    .foregroundStyle(Palette.inkFaintest)
                    .padding(.horizontal, Space.scaled(9))
                    .padding(.top, Space.s4)
            }
        }
    }
#endif
