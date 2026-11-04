// The calendar page's detail for one row: the full time, where it is, how
// real it is and why, and the mail behind it, which is the event's index into
// the mailbox (docs/CALENDAR.md §9). Shown beside the agenda on a wide window
// and in place under the row on a narrow one; the same view either way.
//
// MODEL TEXT IS QUOTED, NOT SPOKEN. An estimate's basis and a likelihood's
// reason are the triage model's words about somebody's mail. They are set in
// quotation marks with a line saying whose they are, so nothing a model wrote
// reads as Passband stating a fact. Everything else here is derived from the
// wire's own fields and is said plainly.
//
// THE PHONE PUSHES THIS SAME VIEW (PassbandiOS/Views/MobileCalendarView.swift).
// What differs there is only the verbs: no key chips on a phone, and edit
// opens the phone's sheet (PassbandiOS/Views/MobileEventSheet.swift), and
// delete asks on the phone's own page (MobileEventPage).

import MapKit
import SwiftUI

struct CalendarDetail: View {
    @Environment(AppStore.self) private var store
    let item: AgendaItem
    let calendar: Calendar
    /// The card's picture once it is known (Model/EventVisuals.swift).
    @State private var visual: EventVisual?
    @State private var choosingMap = false
    @Environment(\.openURL) private var openURL

    var body: some View {
        VStack(alignment: .leading, spacing: Space.s12) {
            header
            if let visual {
                EventVisualView(visual: visual)
            }
            VStack(alignment: .leading, spacing: Space.s4) {
                Text(item.title)
                    .font(Typo.ui(17, weight: .semibold))
                    .foregroundStyle(Palette.ink)
                    .strikethrough(item.cancelled, color: Palette.inkFaint)
                    .fixedSize(horizontal: false, vertical: true)
                    .textSelection(.enabled)
                whenLine
                if item.endEstimated {
                    estimateNote
                }
            }
            facts
            // The tickets, boarding pass or booking the mail holds: one tap
            // to what the door wants to see.
            if let tickets = item.tickets {
                TicketsRow(tickets: tickets) { store.openTickets(tickets) }
            }
            // A ghost's own mail, or the mail behind the ghost matched to a
            // real event (Gmail's reservation event, say).
            if !item.sources.isEmpty {
                MailBehind(sources: item.sources)
            }
            actions
        }
        .padding(Space.s16)
        .frame(maxWidth: .infinity, alignment: .leading)
        .passbandGlass(.pane, cornerRadius: Radius.pane, tint: Palette.glassTint)
        .animation(.easeOut(duration: 0.2), value: visual != nil)
        .task(id: visualKey) {
            let visuals = EventVisuals.shared
            if let known = visuals.cached(visualKey) {
                visual = known
                return
            }
            visual = nil
            let found = await visuals.resolve(
                key: visualKey, threadIds: item.sources.map(\.thread_id), place: item.placeQuery)
            if !Task.isCancelled { visual = found }
        }
    }

    /// What the picture depends on: the row, its place and its mail.
    private var visualKey: String {
        ([item.id, item.placeQuery ?? ""] + item.sources.map(\.thread_id))
            .joined(separator: "\u{1F}")
    }

    // MARK: header

    private var header: some View {
        HStack(spacing: Space.scaled(7)) {
            AgendaMark(
                fill: item.fill, estimated: item.endEstimated, thin: item.cancelled,
                hue: ChipColors.inks(item).edge)
                .frame(width: 10, height: 10)
            // A real event's calendar is named in its own colour: the
            // same ink its time wears on the month.
            Text(source)
                .font(Typo.micro)
                .foregroundStyle(item.event != nil ? ChipColors.inks(item).ink : Palette.inkFaint)
            Spacer(minLength: Space.s8)
            if item.isGhost {
                Text("not on your calendar")
                    .font(Typo.micro)
                    .foregroundStyle(Palette.inkFaintest)
            }
        }
    }

    /// Where the row came from: the calendar it is on, or what kind of
    /// mail made it.
    private var source: String {
        if let event = item.event {
            let name = event.calendar_name.map { "\($0) calendar" } ?? "Your calendar"
            if event.passband_owned == true { return "\(name), kept current by Passband" }
            return event.from_gmail == true ? "\(name), added by Gmail" : name
        }
        switch item.ghost?.kind ?? .other {
        case .ticket: return "Ticket, from your mail"
        case .travel: return "Travel, from your mail"
        case .reservation: return "Reservation, from your mail"
        case .invite: return "Invitation, from your mail"
        case .proposal: return "Proposed time, from your mail"
        case .other: return "From your mail"
        }
    }

    // MARK: time

    /// "Thu Oct 8, 8pm to ~11pm": the tilde and the guessed end in the dim
    /// inks, the known start at full strength.
    private var whenLine: some View {
        let full = Agenda.fullTime(item, calendar: calendar)
        return Group {
            if let end = full.end {
                if item.endEstimated {
                    Text("\(full.lead) to \(Text("~").foregroundStyle(Palette.inkFaintest))\(Text(end).foregroundStyle(Palette.inkDim))")
                } else {
                    Text("\(full.lead) to \(end)")
                }
            } else {
                Text(full.lead)
            }
        }
        .font(Typo.ui(13))
        .foregroundStyle(Palette.ink)
    }

    /// Where the guess came from. The tier is a fact about the data and
    /// is said plainly; the model's own basis, when it gave one, is
    /// quoted.
    @ViewBuilder
    private var estimateNote: some View {
        let ghost = item.ghost
        VStack(alignment: .leading, spacing: Space.s4) {
            Text(estimateSentence(ghost?.end_source))
                .font(Typo.micro)
                .foregroundStyle(Palette.inkFaint)
                .fixedSize(horizontal: false, vertical: true)
            if let basis = ghost?.estimate?.basis, ghost?.end_source == .model {
                ModelQuote(text: basis)
            }
        }
    }

    private func estimateSentence(_ source: GhostEndSource?) -> String {
        switch source {
        case .history: "The end is a guess, from how long past ones like it ran."
        case .model: "The end is a guess, from what the mail says about the event."
        default: "The end is a guess, from how long this kind of event usually runs."
        }
    }

    // MARK: place

    /// The venue's name, its address under it. On iOS a tap asks which maps
    /// app to open it in; on the Mac it stays selectable text.
    @ViewBuilder
    private func placeValue(_ name: String) -> some View {
        let lines = VStack(alignment: .leading, spacing: Space.s2) {
            Text(name)
                .font(Typo.rowSub)
                .foregroundStyle(Palette.ink)
            if let address = item.placeAddress {
                Text(address)
                    .font(Typo.micro)
                    .foregroundStyle(Palette.inkDim)
            }
        }
        #if os(iOS)
            Button { choosingMap = true } label: { lines.contentShape(Rectangle()) }
                .buttonStyle(.plain)
                .confirmationDialog(name, isPresented: $choosingMap, titleVisibility: .visible) {
                    Button("Open in Apple Maps") { openInAppleMaps() }
                    Button("Open in Google Maps") { open(Self.googleMaps, item.placeQuery) }
                    Button("Cancel", role: .cancel) {}
                }
        #else
            lines.textSelection(.enabled)
        #endif
    }

    private static let appleMaps = "https://maps.apple.com/?q="
    /// Opens the Google Maps app when it is installed (a universal link),
    /// else Google Maps in Safari.
    private static let googleMaps = "https://www.google.com/maps/search/?api=1&query="

    /// The venue Apple Maps already matched for the picture when there is
    /// one, so the pin is the same place; else a search for the place.
    private func openInAppleMaps() {
        switch visual {
        case .lookAround(_, let found), .map(let found):
            found.openInMaps()
        default:
            open(Self.appleMaps, item.placeQuery)
        }
    }

    private func open(_ base: String, _ query: String?) {
        guard let query,
            let encoded = query.addingPercentEncoding(withAllowedCharacters: .urlQueryAllowed.subtracting(["&", "+", "="])),
            let url = URL(string: base + encoded)
        else { return }
        openURL(url)
    }

    // MARK: facts

    private var facts: some View {
        VStack(alignment: .leading, spacing: Space.s8) {
            if let ghost = item.ghost {
                standing(ghost)
            }
            if let place = item.location {
                FactRow(label: "Place") { placeValue(place) }
            }
            if let event = item.event {
                let guests = event.guests ?? []
                // The guest list marks its own organizer and has the
                // user's own answer on their row; the two lines below
                // are for an event that lists nobody.
                if !guests.isEmpty || event.guests_truncated == true {
                    FactRow(label: "Guests") { GuestRows(event: event) }
                }
                if let organizer = event.organizer, !organizer.isEmpty,
                    !guests.contains(where: { $0.organizer == true })
                {
                    FactRow(label: "Organizer") {
                        Text(verbatim: organizer)
                            .font(Typo.rowSub)
                            .foregroundStyle(Palette.ink)
                            .textSelection(.enabled)
                    }
                }
                if let response = event.self_response, !guests.contains(where: { $0.isSelf == true }) {
                    FactRow(label: "You") { plain(responseText(response)) }
                }
                if event.busy == false {
                    FactRow(label: "Shows as") { plain("free") }
                }
                if let notes = event.description?.trimmingCharacters(in: .whitespacesAndNewlines),
                    !notes.isEmpty
                {
                    FactRow(label: "Notes") {
                        // The calendar's own text, laundered by the
                        // daemon: plain Text, never markup.
                        Text(verbatim: notes)
                            .font(Typo.rowSub)
                            .foregroundStyle(Palette.ink)
                            .fixedSize(horizontal: false, vertical: true)
                            .lineLimit(12)
                            .textSelection(.enabled)
                    }
                }
            }
            if item.unverified {
                FactRow(label: "Sender") {
                    VStack(alignment: .leading, spacing: Space.s2) {
                        plain("Not verified")
                        Text("The mail this came from did not pass sender authentication, so Passband cannot confirm who sent it.")
                            .font(Typo.micro)
                            .foregroundStyle(Palette.inkFaint)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }
            }
        }
    }

    /// How real a ghost is: certain (and why), or its band, which names
    /// itself beside its own swatch, with the model's reason quoted.
    /// The words are `Agenda.standing`'s, pure and tested.
    @ViewBuilder
    private func standing(_ ghost: TimelineGhost) -> some View {
        switch Agenda.standing(ghost) {
        case .status(let text):
            FactRow(label: "Status") {
                plain(text).fixedSize(horizontal: false, vertical: true)
            }
        case .likelihood(let likelihood):
            FactRow(label: "Likelihood") {
                VStack(alignment: .leading, spacing: Space.s4) {
                    HStack(spacing: Space.s6) {
                        AgendaMark(
                            fill: item.fill, thin: likelihood.band == .unlikely,
                            hue: ChipColors.inks(item).edge)
                            .frame(width: 10, height: 10)
                        Text(likelihood.band.rawValue.capitalized)
                            .font(Typo.ui(12, weight: .semibold))
                            .foregroundStyle(Palette.ink)
                    }
                    if let reason = likelihood.reason {
                        ModelQuote(text: reason)
                    }
                }
            }
        case nil:
            EmptyView()
        }
    }

    private func responseText(_ r: TimelineSelfResponse) -> String {
        switch r {
        case .accepted: "Accepted"
        case .declined: "Declined"
        case .tentative: "Tentative"
        case .needsAction: "Not answered yet"
        }
    }

    private func plain(_ s: String) -> some View {
        Text(s).font(Typo.rowSub).foregroundStyle(Palette.ink)
    }

    // MARK: actions

    /// The verbs, each wearing its key the way the app teaches keys in
    /// place. A ghost: mark going, dismiss, or (on a ghost already marked
    /// going) take the mark off. A real event: open it in Google
    /// Calendar, and, only where the daemon says the event is the user's
    /// to change, edit and delete. An RSVP that answers through the
    /// calendar is a later wave; nothing here writes mail.
    @ViewBuilder
    private var actions: some View {
        if let ghost = item.ghost {
            HStack(spacing: Space.s14) {
                if let source = ghost.sources.last {
                    HintButton(key: "↵", label: "open mail") {
                        store.openThread(
                            source.thread_id, focusMessage: source.message_id, from: .calendar)
                    }
                }
                HintButton(key: "y", label: ghost.state == .going ? "not going" : "going") {
                    let state: GhostState? = ghost.state == .going ? nil : .going
                    Task { await store.markGhost(ghost, state, viaKey: false) }
                }
                HintButton(key: "x", label: "dismiss") {
                    Task { await store.markGhost(ghost, .dismissed, viaKey: false) }
                }
                Spacer(minLength: 0)
            }
        } else if let event = item.event {
            let link = event.html_link.flatMap { Opener.isHTTP($0) ? $0 : nil }
            let editable = CalendarWrite.canEdit(event) && !FixtureMode.isEnabled
            if link != nil || editable {
                HStack(spacing: Space.s14) {
                    if editable {
                        HintButton(key: "e", label: "edit") { store.openEventEditor(editing: event) }
                        HintButton(key: "⌫", label: "delete") { store.askDeleteEvent(event) }
                    }
                    if let link {
                        HintButton(key: "↵", label: "open in Google Calendar") { Opener.open(link) }
                    }
                    Spacer(minLength: 0)
                }
            }
        }
    }
}

// MARK: - pieces

/// A label and its value, the label in a fixed column.
private struct FactRow<Value: View>: View {
    let label: String
    @ViewBuilder var value: Value

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: Space.s10) {
            Text(label)
                .font(Typo.micro)
                .foregroundStyle(Palette.inkFaint)
                .frame(width: Typo.measure(70), alignment: .leading)
            value
                .frame(maxWidth: .infinity, alignment: .leading)
        }
    }
}

/// Who is on an event (wave 5): a name, how they answered, and the
/// organizer marked, each in words. Read-only: guests are changed in the
/// event pane, and only where the daemon says they are the user's to
/// change. The list is Google's text about other people: plain Text.
private struct GuestRows: View {
    let event: TimelineEvent

    var body: some View {
        let list = EventGuests.rows(event)
        VStack(alignment: .leading, spacing: Space.s4) {
            ForEach(list.rows) { row in
                HStack(alignment: .firstTextBaseline, spacing: Space.s8) {
                    Text(verbatim: row.name)
                        .font(Typo.rowSub)
                        .foregroundStyle(Palette.ink)
                        .lineLimit(1)
                        .truncationMode(.middle)
                        .textSelection(.enabled)
                    Text(row.organizer ? "organizer, \(row.response)" : row.response)
                        .font(Typo.micro)
                        // The answers given read a step stronger than
                        // the one still owed; no colour, which on this
                        // page belongs to calendars.
                        .foregroundStyle(row.response == "no reply" ? Palette.inkFaintest : Palette.inkFaint)
                        .fixedSize()
                    Spacer(minLength: 0)
                }
            }
            if let more = list.more {
                Text(more)
                    .font(Typo.micro)
                    .foregroundStyle(Palette.inkFaintest)
            }
        }
    }
}

/// The triage model's words, in quotation marks, with a line saying whose
/// they are. No bar down one side: on a rounded card that reads as a
/// focus ring someone forgot to finish.
struct ModelQuote: View {
    let text: String

    var body: some View {
        VStack(alignment: .leading, spacing: Space.s2) {
            Text(verbatim: "\u{201C}\(text)\u{201D}")
                .font(Typo.rowSub)
                .foregroundStyle(Palette.inkDim)
                .fixedSize(horizontal: false, vertical: true)
                .textSelection(.enabled)
            Text("the triage model's words")
                .font(Typo.micro)
                .foregroundStyle(Palette.inkFaintest)
        }
    }
}

/// A key hint you can also click: the chip, then the verb, the verb
/// turning accent on hover (no shape of its own, like `TextActionStyle`).
struct HintButton: View {
    let key: String
    let label: String
    let action: () -> Void
    @State private var hovering = false

    var body: some View {
        #if os(macOS)
            Button(action: action) {
                HStack(spacing: Space.s4) {
                    Kbd(key)
                    Text(label)
                        .font(Typo.micro)
                        .foregroundStyle(hovering ? Palette.accent : Palette.inkFaint)
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .onHover { hovering = $0 }
            .pointingHand()
        #else
            // No keys on a phone: the verb alone, at a thumb's size.
            Button(action: action) {
                Text(label)
                    .font(Typo.ui(14, weight: .medium))
                    .foregroundStyle(Palette.accent)
                    .padding(.vertical, Space.s6)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
        #endif
    }
}

/// The Tickets button (docs/CALENDAR.md, "The Tickets button"), named by
/// what the mail holds: "Tickets", "Boarding pass", "Booking". Beside it,
/// what a tap opens: the file by its name, or the email. One tap, no menu:
/// an attachment opens in Quick Look (on the phone a wallet pass offers
/// Wallet), the email in the reader.
private struct TicketsRow: View {
    let tickets: CalendarTickets
    let action: () -> Void

    var body: some View {
        HStack(spacing: Space.s10) {
            Button(action: action) {
                Label(tickets.label, systemImage: symbol)
                    .font(Typo.ui(13, weight: .semibold))
            }
            .buttonStyle(.glass)
            #if os(macOS)
                .controlSize(.regular)
                .pointingHand()
            #else
                .controlSize(.large)
            #endif
            .accessibilityHint(hint)
            // The file's name is the sender's: plain Text, never markup.
            Text(verbatim: detail)
                .font(Typo.micro)
                .foregroundStyle(Palette.inkFaint)
                .lineLimit(1)
                .truncationMode(.middle)
            Spacer(minLength: 0)
        }
    }

    private var symbol: String {
        switch tickets.kind {
        case .travel: "airplane"
        case .reservation: "bookmark"
        default: "ticket"
        }
    }

    private var detail: String {
        guard let file = tickets.attachment else { return "in the email" }
        #if os(iOS)
            if file.isWalletPass { return "Add to Wallet" }
        #endif
        return file.filename
    }

    private var hint: String {
        tickets.attachment.map { "Opens \($0.filename)" } ?? "Opens the email"
    }
}

/// "Mail behind it": every message that made, changed, cancelled or
/// confirmed the ghost, oldest first, each opening its thread in the
/// reader the way a sitrep record does.
private struct MailBehind: View {
    let sources: [GhostSource]

    var body: some View {
        VStack(alignment: .leading, spacing: Space.s6) {
            Text("Mail behind it")
                .font(Typo.micro)
                .foregroundStyle(Palette.inkFaint)
            VStack(spacing: 1) {
                ForEach(sources, id: \.message_id) { SourceRow(source: $0) }
            }
        }
        .padding(Space.s10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            RoundedRectangle(cornerRadius: Radius.well, style: .continuous)
                .fill(Palette.hairline.opacity(0.45))
        )
    }
}

/// One message behind a ghost. The wire names only its ids, so the
/// sender and subject come from the thread itself, through the same
/// prefetch cache the reader opens from, which also makes the click
/// instant.
private struct SourceRow: View {
    @Environment(AppStore.self) private var store
    let source: GhostSource
    @State private var message: ClientMessage?
    @State private var subject: String?

    var body: some View {
        Button {
            store.openThread(source.thread_id, focusMessage: source.message_id, from: .calendar)
        } label: {
            HStack(alignment: .firstTextBaseline, spacing: Space.s8) {
                VStack(alignment: .leading, spacing: Space.s2) {
                    // Email-derived: plain Text, never markup.
                    Text(verbatim: subject ?? "Email")
                        .font(Typo.ui(12, weight: .medium))
                        .foregroundStyle(Palette.ink)
                        .lineLimit(1)
                    Text(verbatim: byline)
                        .font(Typo.micro)
                        .foregroundStyle(Palette.inkFaint)
                        .lineLimit(1)
                }
                Spacer(minLength: Space.s8)
                Text(roleWord)
                    .font(Typo.micro)
                    .foregroundStyle(source.role == .cancelled ? Palette.danger : Palette.inkFaintest)
            }
            .padding(.horizontal, Space.s6)
            .padding(.vertical, Space.s4)
            .contentShape(Rectangle())
        }
        .buttonStyle(RecordRowStyle())
        .warmsThreadOnHover(source.thread_id)
        .task(id: source.message_id) {
            guard
                let view = try? await ThreadPrefetch.shared.fetch(
                    source.thread_id, fresh: ThreadPrefetch.warmFresh)
            else { return }
            let m = view.messages.first { $0.id == source.message_id } ?? view.messages.last
            message = m
            subject = m?.subject ?? view.subject
        }
    }

    private var byline: String {
        guard let message else { return " " }
        let who = SenderCache.resolved(message.senderString).displayName
        let when = Fmt.shortDate(message.received_at)
        return when.isEmpty ? who : "\(who), \(when)"
    }

    private var roleWord: String {
        switch source.role {
        case .created: "first"
        case .updated: "update"
        case .cancelled: "cancellation"
        case .agreed: "confirmation"
        }
    }
}
