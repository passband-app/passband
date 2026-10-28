// AROUND THE EVENT PANE: the one-question card before an event is deleted
// from the detail pane, and the two pieces the pane and that card share (the
// question's words, and a calendar named in its colour).
//
// The editor itself was a modal card here in wave 3. It is a pane now
// (Views/CalendarEventPane.swift, wave 4), beside the month instead of over
// it. Deleting is still asked once, and the card that asks from the detail
// pane (⌫, or its "delete") is still a card: a question about one event,
// with nothing to work beside.

#if os(macOS)
    import SwiftUI

    // MARK: - the delete question, on its own

    /// ⌫ or the detail's "delete" on an event: the same question as the
    /// event pane's footer, on a card of its own. Asked once; Enter deletes,
    /// Escape keeps it.
    ///
    /// AN EVENT WITH GUESTS (wave 5) asks "Delete and tell 3 guests?" and
    /// offers both answers by name. Google's cancellation mail cannot be
    /// unsent, so plain Enter answers nothing then: "delete and tell guests"
    /// is a click or ⌘↵, and "delete without telling" is a click.
    struct CalendarDeleteConfirm: View {
        @Environment(AppStore.self) private var store
        let ask: CalendarDeleteAsk

        var body: some View {
            let wording = EventGuests.deleteWording(ask.event, feed: store.calendarFeed)
            OverlayScrim(onDismiss: { store.cancelDeleteEvent() }) {
                ModalCard(width: wording.tell == nil ? 380 : 460) {
                    DeleteQuestion(wording: wording)
                    EventStub(event: ask.event)
                    ModalButtons {
                        Button("Keep it") { store.cancelDeleteEvent() }
                            .buttonStyle(.glass)
                        if let tell = wording.tell {
                            Button(Self.sentence(wording.quiet)) { deleteNow(tellGuests: false) }
                                .buttonStyle(.glass)
                                .disabled(store.calendarWriting)
                            Button { deleteNow(tellGuests: true) } label: {
                                HStack(spacing: Space.s6) {
                                    Text(store.calendarWriting ? "Deleting…" : Self.sentence(tell))
                                    Text("⌘↵")
                                        .font(Typo.mono(10, weight: .medium))
                                        .opacity(0.6)
                                }
                            }
                            .buttonStyle(.glassProminent)
                            .tint(Palette.danger)
                            .disabled(store.calendarWriting)
                        } else {
                            Button(store.calendarWriting ? "Deleting…" : "Delete event") {
                                deleteNow(tellGuests: false)
                            }
                            .buttonStyle(.glassProminent)
                            .tint(Palette.danger)
                            .disabled(store.calendarWriting)
                        }
                    }
                }
            }
            .keyContext(.modal)
            .keyBindings(.modal, [
                KeyBinding("Escape", "keep it", allowInInput: true) { store.cancelDeleteEvent() },
                // Swallowed when there are guests to tell: Enter alone
                // never has Google email anyone.
                KeyBinding("Enter", "delete", allowInInput: true) {
                    if wording.tell == nil { deleteNow(tellGuests: false) }
                },
                KeyBinding("Enter", "delete and tell guests", meta: true, allowInInput: true) {
                    deleteNow(tellGuests: wording.tell != nil)
                },
            ])
        }

        /// The card's buttons start with a capital, as its others do.
        private static func sentence(_ s: String) -> String {
            s.prefix(1).uppercased() + s.dropFirst()
        }

        /// `tellGuests` is true from "Delete and tell guests" and its ⌘↵,
        /// and from nowhere else.
        private func deleteNow(tellGuests: Bool) {
            guard !store.calendarWriting else { return }
            let event = ask.event
            Task { await store.deleteEvent(event, tellGuests: tellGuests) }
        }
    }

    /// The question both delete paths ask, in the same words: by the
    /// event's guests (`EventGuests.deleteWording`).
    struct DeleteQuestion: View {
        let wording: DeleteWording

        var body: some View {
            VStack(alignment: .leading, spacing: Space.s4) {
                Text(wording.title)
                    .font(Typo.ui(13, weight: .semibold))
                    .foregroundStyle(Palette.ink)
                    .fixedSize(horizontal: false, vertical: true)
                Text(wording.detail)
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
            .background(Palette.hairline.opacity(0.6), in: RoundedRectangle(cornerRadius: Radius.well, style: .continuous))
        }
    }

    /// A calendar named in its colour: a swatch and the name, on a capsule
    /// that fills with the calendar's tint and takes an even edge when it is
    /// the one picked.
    struct CalendarTag: View {
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
                    .frame(maxWidth: Typo.measure(200), alignment: .leading)
            }
            .padding(.horizontal, Space.s8)
            .padding(.vertical, Space.s4)
            .background(Capsule().fill(inks.fill.opacity(selected ? 1 : 0)))
            .overlay(
                Capsule().strokeBorder(
                    selected ? inks.edge.opacity(0.8) : Palette.hairlineStrong, lineWidth: 1))
            .contentShape(Capsule())
        }
    }
#endif
