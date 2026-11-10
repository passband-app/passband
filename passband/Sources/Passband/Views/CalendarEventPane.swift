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
// THE TITLE READS ITSELF (wave 6, Lib/TitleParse.swift). On a NEW event,
// "dinner with steven on friday 4pm" fills the day and the times as it is
// typed (the wells it filled wear a soft accent wash until the user types in
// them), and a line under the title says what will be saved: "Dinner with
// Steven". A well the user set by hand is never written again; deleting the
// words puts back what was there; Esc on the title (or ⌘Z) takes the title as
// typed. "with steven" also asks the daemon who that most likely is and
// OFFERS them on the guests line, in a dashed box with a checkmark and an x:
// not a guest, and not counted by anything, until the checkmark.
//
// THE END DAY. A timed event shows its end day beside its end time, always.
// Left alone it follows the start (the next day when the end time is at or
// before the start) and is drawn dim; typed into, it is the user's and stops
// following.
//
// KEYS. Tab from the title goes to a suggestion that has been showing for a
// moment (an answer landing as Tab is pressed does not move where Tab goes);
// ↵ on it adds the guest, ⌫ or x dismisses it. A suggestion becomes a guest
// only by its checkmark or by ↵ with the keyboard on it.
// ⌘↵ presses the footer's main control from anywhere in the pane. ↵
// saves from a single-line field ONLY when the save tells nobody; it also
// picks a highlighted place or contact, makes a typed address a guest, and is
// a new line in the notes. With no field focused it is the month's. Esc
// closes the place list, then the delete question, then the pane.
//
// FROZEN (Lib/CalendarWrite.swift, `EventPaneState`). A save whose answer
// never came and that the reloaded page cannot confirm leaves the pane
// read-only, the draft as sent still showing, with one line and three
// controls in the footer: "check again" (⌘↵), "send again" (the same body),
// "close" (Esc). Plain ↵ does nothing. There is no way back to editing: a
// changed event is a new pane.

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
        /// Who the daemon says each name in the title most likely is, by the
        /// name's key. An empty list is "asked, nobody". Filled behind the
        /// pane; nothing waits on it.
        @State private var namedPeople: [String: [NamedPerson]] = [:]
        /// The keyboard is on the guest suggestion (Tab from the title). No
        /// text field has the caret while it is.
        @State private var onSuggestion = false
        /// When the suggestion now showing came up: Tab takes it only once
        /// it has been there to be seen (`EventGuests.tabTakesSuggestion`).
        @State private var suggestionSince: Date?
        @FocusState private var focus: Field?

        private enum Field: Hashable {
            case title, date, endDate, start, end, guests, location, notes
        }

        private struct GuestPlace: Equatable {
            let name: String
            let zone: TimeZone?
        }

        private var draft: EventDraft { store.calendarDraft ?? request.initial }
        /// Where this pane's write stands. Typing only while `.editing`.
        private var state: EventPaneState { store.calendarPaneState }
        private var editing: TimelineEvent? { request.event }

        var body: some View {
            // "Today" for a typed "fri" or "tomorrow", read once a render.
            let now = Date()
            let cal = Calendar.current
            let plan = store.guestPlan(draft, request)
            let problem = blocking(plan: plan, now: now, cal: cal)
            let reading = EventForm.titleReading(draft, today: now, calendar: cal)
            let suggestion = EventGuests.suggestion(
                draft, reading: reading, access: plan.access, people: namedPeople,
                owner: store.calendarAccountEmail)
            VStack(alignment: .leading, spacing: 0) {
                header
                card(plan: plan, problem: problem, reading: reading, suggestion: suggestion, now: now, cal: cal)
                    // A write out, or one in doubt: the draft stays in view
                    // as it was sent, and nothing in it takes typing.
                    .disabled(!state.fieldsEditable)
                    // The composer's edges (`ComposeEdge`): the card on the
                    // panel gutter, the header's and footer's words on the
                    // card's text line.
                    .padding(.horizontal, ComposeEdge.card)
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
            .keyBindings(.modal, bindings(problem: problem, plan: plan, suggestion: suggestion, now: now, cal: cal))
            // Where each guest is, asked of the daemon as they are added.
            .task(id: guestKeys(plan)) { await lookUpGuests(plan) }
            // Who the title's names most likely are, asked as they are typed.
            .task(id: EventGuests.namesToAsk(draft, reading: reading, access: plan.access)) {
                await lookUpNames(EventGuests.namesToAsk(draft, reading: reading, access: plan.access))
            }
            // The suggestion went (answered, or its name deleted): the
            // keyboard goes back to the title it came from.
            // Another offer in its place (the next name's): the same, so ↵
            // never adds somebody the keyboard was not put on.
            .onChange(of: suggestion) { was, now in
                if was?.name != now?.name || was?.best != now?.best {
                    suggestionSince = now == nil ? nil : Date()
                }
                if onSuggestion, !EventGuests.keyboardStays(was: was, now: now) {
                    onSuggestion = false
                    focus = .title
                }
            }
            .onAppear {
                let lookup = PlaceLookup(provider: MapKitPlaces())
                lookup.write = { text in patch { $0.location = text } }
                places = lookup
                // Not from under the assistant's field, and NEVER for a pane
                // the assistant filled in: it takes the keyboard only when
                // the user clicks into it (`EventProposals.keysMayAct`).
                if !store.modalOverlayOpen, keysAreTheUsers { focus = .title }
            }
            // A click anywhere in a proposed pane makes its keys the user's.
            .simultaneousGesture(TapGesture().onEnded { store.engageProposedPane() })
            .onDisappear { places?.stop() }
            // A click elsewhere defocuses the line; a list about text the
            // caret has left must not linger.
            .onChange(of: focus) { _, now in
                if now != .location { places?.dismiss() }
                // A caret anywhere is off the suggestion.
                if now != nil { onSuggestion = false }
                // A caret put in a field BY A CLICK is the user in the pane
                // (a text field takes the click before the tap gesture above
                // sees it). Never by Tab: that can be typed ahead.
                if now != nil, NSApp.currentEvent?.type == .leftMouseDown { store.engageProposedPane() }
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
            .padding(.horizontal, ComposeEdge.text)
            .padding(.vertical, Space.scaled(13))
            .overlay(alignment: .bottom) { Hairline() }
        }

        // MARK: the card

        /// ONE CARD, as the composer's is: the title, then the labelled lines
        /// ruled apart, then the notes on the same ground taking what is left.
        private func card(
            plan: GuestPlan, problem: EventDraftProblem?, reading: TitleReading?, suggestion: GuestSuggestion?,
            now: Date, cal: Calendar
        ) -> some View {
            let savesChanged = EventForm.titleSavesChanged(draft, today: now, calendar: cal)
            return VStack(alignment: .leading, spacing: 0) {
                TextField("Title", text: titleBinding(now: now, cal: cal))
                    .textFieldStyle(.plain)
                    .font(Typo.ui(15, weight: .semibold))
                    .foregroundStyle(Palette.ink)
                    .focused($focus, equals: .title)
                    .padding(.horizontal, InlineFieldMetrics.inset)
                    .padding(.top, Space.s10)
                    .padding(.bottom, savesChanged ? Space.s2 : Space.s10)
                // Only when words came out of the title. A reading that
                // fills the wells and keeps every word shows on the wells.
                if let reading, savesChanged {
                    savedTitleLine(reading)
                }
                Hairline()
                InlineFieldRow(label: "calendar") { calendarPicker }
                Hairline()
                InlineFieldRow(label: "when") { whenFields(now: now, cal: cal) }
                guestsSection(plan: plan, problem: problem, suggestion: suggestion, now: now, cal: cal)
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
                RoundedRectangle(cornerRadius: Radius.well, style: .continuous)
                    .fill(Palette.canvas.opacity(0.65))
            )
            .overlay(
                RoundedRectangle(cornerRadius: Radius.well, style: .continuous)
                    .strokeBorder(Palette.hairlineStrong, lineWidth: 0.75))
        }

        /// Under the title, while words are coming out of it ("on friday
        /// 4pm"): the title that will be saved, and the way to keep the typed
        /// one instead. Never an empty one: words are not taken out of a
        /// title that would be left with none.
        private func savedTitleLine(_ reading: TitleReading) -> some View {
            HStack(alignment: .firstTextBaseline, spacing: Space.s6) {
                Text("saves as")
                    .foregroundStyle(Palette.inkFaintest)
                // The user's own words: plain Text, never markup.
                Text(verbatim: reading.title)
                    .foregroundStyle(Palette.inkDim)
                    .lineLimit(1)
                    .truncationMode(.tail)
                Spacer(minLength: Space.s8)
                KeyHint("esc", "keep as typed")
                    .fixedSize()
            }
            .font(Typo.micro)
            .padding(.horizontal, InlineFieldMetrics.inset)
            .padding(.bottom, Space.s8)
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
                .padding(.vertical, -Space.s2)
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

        /// The day and the times as typed wells, the all-day switch, and
        /// under them what they resolve to. A timed event reads start day,
        /// start time, end time, end day: one line where the pane is wide
        /// enough, the end under the start where it is not.
        private func whenFields(now: Date, cal: Calendar) -> some View {
            VStack(alignment: .leading, spacing: Space.s6) {
                ViewThatFits(in: .horizontal) {
                    HStack(spacing: Space.s6) {
                        dayWell
                        if draft.allDay {
                            joiner("through")
                            lastDayWell
                        } else {
                            startWell
                            joiner("to")
                            endWell
                            endDayWell(now: now, cal: cal)
                        }
                        Spacer(minLength: Space.s8)
                        allDayToggle
                    }
                    VStack(alignment: .leading, spacing: Space.s6) {
                        HStack(spacing: Space.s6) {
                            dayWell
                            if !draft.allDay { startWell }
                            Spacer(minLength: Space.s8)
                            allDayToggle
                        }
                        HStack(spacing: Space.s6) {
                            if draft.allDay {
                                joiner("through")
                                lastDayWell
                            } else {
                                joiner("to")
                                endWell
                                endDayWell(now: now, cal: cal)
                            }
                        }
                    }
                }
                whenLine(now: now, cal: cal)
            }
            // The wells are taller than a line of text; pulled up so the
            // first of them centres on the label beside it.
            .padding(.vertical, -Space.scaled(3))
        }

        private var dayWell: some View {
            well("Day", \.date, .date, width: Typo.measure(112), fills: .date)
        }

        /// The times are typed AND picked: each well keeps its caret, and a
        /// chevron inside it opens half hours to choose from (the end's
        /// counted from the start, with how long that makes it).
        private var startWell: some View {
            well(
                "Start", \.start, .start, width: Typo.measure(70) + Self.chevronRoom, fills: .start,
                choices: EventForm.startChoices(draft.start))
        }

        private var endWell: some View {
            well(
                "End", \.end, .end, width: Typo.measure(70) + Self.chevronRoom, fills: .end,
                choices: EventForm.endChoices(start: draft.start, end: draft.end))
        }

        private static var chevronRoom: CGFloat { Space.s14 }

        /// An all-day run's last day; empty is the same day.
        private var lastDayWell: some View {
            well("Same day", \.endDate, .endDate, width: Typo.measure(112))
        }

        /// THE END DAY OF A TIMED EVENT, always there to be changed. While it
        /// is empty it FOLLOWS: the well shows, in the dim ink, the day the
        /// event will end on (the start's, or the next when the end time is
        /// at or before the start). A day typed into it is the user's own and
        /// stops following; emptied, it follows again.
        private func endDayWell(now: Date, cal: Calendar) -> some View {
            well(
                EventForm.impliedEndDate(draft, today: now, calendar: cal) ?? "End day", \.endDate, .endDate,
                width: Typo.measure(112), promptTone: Palette.inkFaint)
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
        ///
        /// `fills`: the when field this well is to the title. Typing in it
        /// makes it the user's own (`EventForm.handSet`), and while the title
        /// is what filled it the well wears a soft accent wash: inferred, and
        /// said so without a word.
        private func well(
            _ placeholder: String, _ keyPath: WritableKeyPath<EventDraft, String>, _ field: Field, width: CGFloat,
            fills: TitleField? = nil, promptTone: Color = Palette.inkFaintest, choices: [EventForm.TimeChoice]? = nil
        ) -> some View {
            let shape = RoundedRectangle(cornerRadius: Radius.well, style: .continuous)
            let inferred = fills.map { draft.titleRead.filled[$0] != nil } ?? false
            let text = bind(keyPath, hand: fills)
            return HStack(spacing: Space.s2) {
                TextField("", text: text, prompt: Text(placeholder).foregroundStyle(promptTone))
                    .textFieldStyle(.plain)
                    .font(Typo.ui(13))
                    .monospacedDigit()
                    .foregroundStyle(Palette.ink)
                    .focused($focus, equals: field)
                if let choices {
                    timeMenu(choices, placeholder: placeholder) { text.wrappedValue = $0 }
                }
            }
                .padding(.horizontal, Space.scaled(7))
                .padding(.vertical, Space.scaled(3))
                .frame(width: width)
                .background(shape.fill(inferred ? Palette.accentSoft : Palette.hairline.opacity(0.5)))
                .help(inferred ? "From the title. Type here to set it yourself." : "")
                // The well being typed in takes an even edge, all the way round.
                .overlay(shape.strokeBorder(Palette.accent.opacity(focus == field ? 0.55 : 0), lineWidth: 1))
        }

        /// The chevron at a time well's end: a menu of `choices`, the well's
        /// own time ticked. A pick writes the time as if it were typed.
        private func timeMenu(
            _ choices: [EventForm.TimeChoice], placeholder: String, pick: @escaping (String) -> Void
        ) -> some View {
            Menu {
                ForEach(choices, id: \.minutes) { c in
                    Toggle(c.label, isOn: Binding(get: { c.current }, set: { _ in pick(c.text) }))
                }
            } label: {
                Image(systemName: "chevron.down")
                    .font(Typo.ui(8, weight: .semibold))
                    .foregroundStyle(Palette.inkFaint)
            }
            .menuStyle(.borderlessButton)
            .menuIndicator(.hidden)
            .fixedSize()
            .pointingHand()
            .help("Pick a time")
            .accessibilityLabel("\(placeholder) time choices")
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
        /// (9 to 10, as a new event's), and lets the end day follow the start
        /// again: a five-day run does not become a five-day meeting. The
        /// title is read again either way, since an all-day event takes no
        /// time from it.
        private func setAllDay(_ on: Bool) {
            let now = Date()
            patch { d in
                d.allDay = on
                if !on {
                    if EventForm.trimmed(d.start).isEmpty { d.start = "9am" }
                    if EventForm.trimmed(d.end).isEmpty { d.end = "10am" }
                    d.endDate = ""
                }
                d = EventForm.refilled(d, today: now, calendar: .current)
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
                (text, tone) = (EventForm.summary(w, now: now, calendar: cal), Palette.inkFaint)
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
            plan: GuestPlan, problem: EventDraftProblem?, suggestion: GuestSuggestion?, now: Date, cal: Calendar
        ) -> some View {
            switch plan.access {
            case .none:
                EmptyView()
            case .field:
                Hairline()
                RecipientField(
                    text: bind(\.guests), focus: $focus, field: Field.guests, label: "guests",
                    placeholder: "Add people by email", inline: true)
                if let suggestion {
                    GuestSuggestionBox(
                        suggestion: suggestion, focused: onSuggestion,
                        accept: { accept($0, suggestion) }, dismiss: { dismiss(suggestion) }
                    )
                    .padding(.top, -Space.s2)
                    .modifier(InlineFieldTuck(active: true))
                }
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

        /// Ask the daemon who each name in the title most likely is, the name
        /// as typed first and then its first word. Behind the pane: a daemon
        /// without the route, or one that knows nobody of that name, leaves
        /// an empty answer and no suggestion; a failure is not an answer
        /// (`EventGuests.nameAnswer`) and is asked again the next time the
        /// names change.
        private func lookUpNames(_ names: [String]) async {
            let missing = names.filter { namedPeople[$0] == nil }
            guard !missing.isEmpty else { return }
            // A beat, so "with st", "with ste", "with stev" are one question.
            try? await Task.sleep(for: .milliseconds(450))
            for name in missing {
                var asks: [EventGuests.NameAsk] = []
                for query in EventGuests.queries(for: name) {
                    guard !Task.isCancelled else { return }
                    let ask: EventGuests.NameAsk
                    do {
                        ask = .people(try await APIClient.shared.calendarPeople(named: query))
                    } catch {
                        // Cancelled mid-flight: not an answer.
                        if Task.isCancelled { return }
                        // A daemon older than the route: nobody to offer.
                        // Anything else (the network, a 500) is no answer.
                        ask = (error as? APIError)?.kind == .notFound ? .noRoute : .failed
                    }
                    asks.append(ask)
                    if EventGuests.nameSettled(by: ask) { break }
                }
                guard !Task.isCancelled else { return }
                // A failure is not stored, so the name is still missing and
                // is asked again the next time the names change.
                if let answer = EventGuests.nameAnswer(asks) { namedPeople[name] = answer }
            }
        }

        /// The checkmark (or ↵ on the box): that person is a guest now.
        private func accept(_ person: NamedPerson, _ suggestion: GuestSuggestion) {
            patch { $0 = EventGuests.accepting(person, for: suggestion.name, in: $0) }
        }

        /// The x (or ⌫ on the box): nobody added, and not offered again.
        private func dismiss(_ suggestion: GuestSuggestion) {
            patch { $0 = EventGuests.dismissing(suggestion.name, in: $0) }
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
                if state.frozen {
                    frozenFooter
                } else if confirmingDelete {
                    deleteQuestion
                } else {
                    verbs(problem: problem, plan: plan)
                }
            }
            .padding(.horizontal, ComposeEdge.text)
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
            // A proposal with guests: the button is the only way, so it does
            // not wear the ⌘↵ cap it would not answer to.
            let clickOnly = guestsNeedAClick(plan)
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
                    let label = store.calendarWriting ? "saving…" : controls.primary.label
                    if clickOnly { Text(label) } else { keyed(label, "⌘↵") }
                }
                .buttonStyle(.glassProminent)
                .tint(Palette.accent)
                .disabled(blocked)
            }
            return VStack(alignment: .trailing, spacing: Space.s8) {
                // ENTER DOES NOT SEND, said where the eye is when it matters.
                if clickOnly {
                    Text(EventProposals.guestsNeedAClickHint)
                        .font(Typo.micro)
                        .foregroundStyle(Palette.inkFaintest)
                } else if !EventGuests.enterMaySave(controls) {
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

        /// THE FROZEN FOOTER: what is known, said once, and the three ways on.
        /// The same verbs and keys as every other footer of the pane; no
        /// colour but the main control's, since nothing here is an error.
        private var frozenFooter: some View {
            let live = Set(state.frozenControls)
            let checking = state == .sending(again: true)
            return VStack(alignment: .leading, spacing: Space.s10) {
                Text(checking ? CalendarWrite.frozenCheckingText : CalendarWrite.frozenText)
                    .font(Typo.ui(13))
                    .foregroundStyle(Palette.inkDim)
                    .fixedSize(horizontal: false, vertical: true)
                ViewThatFits(in: .horizontal) {
                    HStack(spacing: Space.s8) {
                        Spacer(minLength: 0)
                        frozenControls(live)
                    }
                    VStack(alignment: .trailing, spacing: Space.s8) { frozenControls(live) }
                        .frame(maxWidth: .infinity, alignment: .trailing)
                }
            }
        }

        @ViewBuilder
        private func frozenControls(_ live: Set<EventPaneState.Control>) -> some View {
            Button { store.closeEventEditor() } label: { keyed(EventPaneState.closeLabel, "esc") }
                .buttonStyle(.glass)
                .disabled(!live.contains(.close))
            if let sent = store.calendarPaneSent {
                Button(EventPaneState.sendAgainLabel(sent)) {
                    Task { await store.sendEventAgain(request) }
                }
                .buttonStyle(.glass)
                .disabled(!live.contains(.sendAgain))
            }
            Button { checkAgain() } label: { keyed(EventPaneState.checkAgainLabel, "⌘↵") }
                .buttonStyle(.glassProminent)
                .tint(Palette.accent)
                .disabled(!live.contains(.checkAgain))
        }

        private func checkAgain() {
            guard state == .uncertain else { return }
            Task { await store.checkEventAgain(request) }
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

        private func bindings(
            problem: EventDraftProblem?, plan: GuestPlan, suggestion: GuestSuggestion?, now: Date, cal: Calendar
        ) -> [KeyBinding] {
            [
                // THE SUGGESTION'S KEYS. Tab from the title lands on it (and
                // on from it, to the day); ↵ adds the person, ⌫ or x says no.
                // All four decline when no suggestion is showing, or the
                // keyboard is not on it, so they stay the fields' own.
                KeyBinding(declining: "Tab", "to the suggested guest", allowInInput: true) {
                    guard !store.modalOverlayOpen, state.fieldsEditable, suggestion != nil else { return false }
                    if onSuggestion {
                        onSuggestion = false
                        focus = .date
                        return true
                    }
                    // Only from the title, and only onto a box that has been
                    // there to be seen: an answer that lands as Tab is
                    // pressed does not change where Tab goes.
                    guard
                        EventGuests.tabTakesSuggestion(
                            caretInTitle: focus == .title,
                            shownFor: suggestionSince.map { Date().timeIntervalSince($0) })
                    else { return false }
                    // The order matters: dropping the caret clears the flag.
                    focus = nil
                    DispatchQueue.main.async { onSuggestion = true }
                    return true
                },
                KeyBinding(declining: "shift+Tab", "back to the title", allowInInput: true) {
                    guard onSuggestion else { return false }
                    onSuggestion = false
                    focus = .title
                    return true
                },
                KeyBinding(declining: "Backspace", "dismiss the suggestion") {
                    guard onSuggestion, let suggestion, state.fieldsEditable else { return false }
                    dismiss(suggestion)
                    return true
                },
                KeyBinding(declining: "x", "dismiss the suggestion") {
                    guard onSuggestion, let suggestion, state.fieldsEditable else { return false }
                    dismiss(suggestion)
                    return true
                },
                // ONE UNDO on a title that was read puts the literal text
                // back in charge, as Esc there does. Any other ⌘Z is the
                // field's own.
                KeyBinding(declining: "z", "keep the title as typed", meta: true, allowInInput: true) {
                    guard !store.modalOverlayOpen, focus == .title, state.fieldsEditable,
                        EventForm.titleIsRead(draft, today: now, calendar: cal)
                    else { return false }
                    patch { $0 = EventForm.literalTitle($0, today: now, calendar: cal) }
                    return true
                },
                // The innermost thing open closes first: the place list, the
                // delete question, then the pane.
                KeyBinding("Escape", "close", allowInInput: true) {
                    // Frozen, or a write out: Esc closes the pane itself.
                    if state.fieldsEditable { escape(now: now, cal: cal) } else { store.closeEventEditor() }
                },
                // ⌘↵ FROM ANYWHERE, the notes included: the footer's main
                // control, by name, exactly as a click on it. Declines under
                // a modal raised over the pane (the ask bar, the shortcuts
                // card), whose field the chord was typed into.
                KeyBinding(declining: "Enter", "save", meta: true, allowInInput: true) {
                    guard !store.modalOverlayOpen else { return false }
                    // A pane the assistant filled in and nobody has clicked
                    // or typed in: the chord does nothing. The button does.
                    guard keysAreTheUsers else {
                        store.noteProposedPaneLocked()
                        return true
                    }
                    switch state.action(.commandEnter) {
                    case .editingRules: break
                    case .checkAgain:
                        checkAgain()
                        return true
                    case .close, .nothing: return true
                    }
                    if confirmingDelete {
                        deleteNow(tellGuests: deleteWording.tell != nil)
                    } else {
                        // GUESTS A MODEL NAMED ARE INVITED BY A CLICK ONLY,
                        // however much of the pane has been clicked or typed
                        // in (`EventProposals.commandEnterMaySave`).
                        guard !guestsNeedAClick(plan) else {
                            store.noteProposedGuestsNeedAClick()
                            return true
                        }
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
                    // As ⌘↵: nothing, on an untouched proposal.
                    guard keysAreTheUsers else {
                        store.noteProposedPaneLocked()
                        return true
                    }
                    // FROZEN, or a write out: ↵ does nothing at all.
                    guard state.action(.enter) == .editingRules else { return true }
                    // On the suggestion: the checkmark. Adding a guest is
                    // not a save, so the rules below never see it.
                    if onSuggestion, let suggestion {
                        accept(suggestion.best, suggestion)
                        return true
                    }
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

        /// The innermost thing first: the place list, the keyboard off the
        /// suggestion and back in the title, a title that was read taken as
        /// typed, the delete question, then the pane.
        private func escape(now: Date, cal: Calendar) {
            if places?.dismiss() == true { return }
            if onSuggestion {
                onSuggestion = false
                focus = .title
                return
            }
            if focus == .title, EventForm.titleIsRead(draft, today: now, calendar: cal) {
                patch { $0 = EventForm.literalTitle($0, today: now, calendar: cal) }
                return
            }
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
            guard problem == nil, !store.calendarWriting, state.fieldsEditable else { return }
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
        ///
        /// `hand`: the when field this is to the title. A change typed here
        /// makes the field the user's own, and the title stops writing it.
        private func bind(_ keyPath: WritableKeyPath<EventDraft, String>, hand: TitleField? = nil) -> Binding<String> {
            Binding(
                get: { (store.calendarDraft ?? request.initial)[keyPath: keyPath] },
                set: { value in
                    guard (store.calendarDraft ?? request.initial)[keyPath: keyPath] != value else { return }
                    patch {
                        $0[keyPath: keyPath] = value
                        if let hand { $0 = EventForm.handSet($0, hand) }
                    }
                })
        }

        /// The title, typed: the draft takes it and its when fields are
        /// filled, or put back, by what it now says (`EventForm.retitled`).
        private func titleBinding(now: Date, cal: Calendar) -> Binding<String> {
            Binding(
                get: { (store.calendarDraft ?? request.initial).title },
                set: { value in
                    guard (store.calendarDraft ?? request.initial).title != value else { return }
                    patch { $0 = EventForm.retitled($0, title: value, today: now, calendar: cal) }
                })
        }

        private func patch(_ mutate: (inout EventDraft) -> Void) {
            // Only while this pane's request is still the one open: a late
            // write (a place looked up after Esc) must not revive a draft.
            // Nor touch a draft that is out, or frozen as it was sent.
            guard store.calendarEditor?.id == request.id, store.calendarPaneState.fieldsEditable,
                var next = store.calendarDraft
            else { return }
            mutate(&next)
            if next != store.calendarDraft {
                store.calendarDraft = next
                // Every write through here is the user's hand in the pane
                // (a field typed in, a chip or a checkmark clicked): a
                // proposed pane's keys are theirs from here.
                store.engageProposedPane()
            }
        }

        /// ↵ and ⌘↵ act, and the pane may take the keyboard: always, except
        /// in a pane the assistant filled in that nobody has clicked or
        /// typed in yet.
        /// The assistant filled this pane in and its save would tell guests:
        /// ⌘↵ never saves it, and the footer says so.
        private func guestsNeedAClick(_ plan: GuestPlan) -> Bool {
            !EventProposals.commandEnterMaySave(
                proposed: store.calendarPaneProposed, engaged: true,
                tellsGuests: !EventGuests.enterMaySave(plan.controls))
        }

        private var keysAreTheUsers: Bool {
            EventProposals.keysMayAct(proposed: store.calendarPaneProposed, engaged: store.calendarPaneEngaged)
        }
    }

    // MARK: - the suggested guest

    /// "with steven" in the title, and who that most likely is: OFFERED, in
    /// a box that is plainly not a guest pill. A pill is a filled capsule; this
    /// is an empty one with a dashed edge (the draft chip's own dash, the
    /// pane's word for "not on the calendar yet"), the person's name and
    /// address inside it, a checkmark to add them and an x to say no. Other
    /// people of the same name follow quietly after "or".
    ///
    /// Names and addresses are mail-derived: plain Text, never markup.
    private struct GuestSuggestionBox: View {
        let suggestion: GuestSuggestion
        /// The keyboard is on it (Tab from the title): an even accent edge.
        let focused: Bool
        let accept: (NamedPerson) -> Void
        let dismiss: () -> Void

        var body: some View {
            let shape = Capsule()
            let best = suggestion.best
            let name = (best.name ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
            VStack(alignment: .leading, spacing: Space.s4) {
                HStack(spacing: Space.s6) {
                    HStack(spacing: Space.s6) {
                        if !name.isEmpty {
                            Text(verbatim: name)
                                .font(Typo.ui(11, weight: .medium))
                                .foregroundStyle(Palette.ink)
                                .lineLimit(1)
                                .layoutPriority(1)
                        }
                        Text(verbatim: best.addr)
                            .font(Typo.mono(11))
                            .foregroundStyle(name.isEmpty ? Palette.ink : Palette.inkFaint)
                            .lineLimit(1)
                            .truncationMode(.middle)
                    }
                    icon("checkmark", tone: Palette.accentInk, help: "Add as a guest ( ↵ )") { accept(best) }
                        .accessibilityLabel("Add \(name.isEmpty ? best.addr : name) as a guest")
                    icon("xmark", tone: Palette.inkFaint, help: "Not them ( ⌫ )", action: dismiss)
                        .accessibilityLabel("Dismiss the suggestion")
                }
                .padding(.leading, Space.s8)
                .padding(.trailing, Space.scaled(3))
                .padding(.vertical, Space.s2)
                .overlay(
                    shape.strokeBorder(
                        focused ? Palette.accent.opacity(0.7) : Palette.inkFaint.opacity(0.55),
                        style: StrokeStyle(lineWidth: 1, dash: focused ? [] : [3, 2.5])))
                .help("Suggested from the title. Not a guest until you add them.")
                if !suggestion.others.isEmpty {
                    HStack(alignment: .firstTextBaseline, spacing: Space.s6) {
                        Text("or")
                            .foregroundStyle(Palette.inkFaintest)
                        ForEach(suggestion.others, id: \.addr) { other in
                            Button {
                                accept(other)
                            } label: {
                                Text(verbatim: label(other))
                                    .lineLimit(1)
                                    .truncationMode(.middle)
                            }
                            .buttonStyle(.textAction)
                            .pointingHand()
                            .help("Add \(other.addr) as a guest instead")
                        }
                    }
                    .font(Typo.micro)
                    .padding(.leading, Space.s8)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .accessibilityElement(children: .contain)
            .accessibilityLabel("Suggested guest")
        }

        /// Another candidate, in a few words: their name with the address's
        /// host when they have one (two Stevens are told apart by where they
        /// write from), or the address.
        private func label(_ person: NamedPerson) -> String {
            let name = (person.name ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
            guard !name.isEmpty else { return person.addr }
            let host = person.addr.split(separator: "@").last.map(String.init) ?? ""
            return host.isEmpty ? name : "\(name) at \(host)"
        }

        private func icon(_ symbol: String, tone: Color, help: String, action: @escaping () -> Void) -> some View {
            Button(action: action) {
                Image(systemName: symbol)
                    .font(Typo.ui(9, weight: .semibold))
                    .foregroundStyle(tone)
                    .frame(width: Space.scaled(20), height: Space.scaled(20))
                    .contentShape(Circle())
            }
            .buttonStyle(.plain)
            .pointingHand()
            .help(help)
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
                        RoundedRectangle(cornerRadius: Radius.row, style: .continuous)
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
