// SEND LATER. The clock icon just left of a composer's send verb, and the time
// palette it opens. Beside the verb because it changes what the verb does: a
// pick turns the icon into a `Tue 9am ×` chip in the same spot, and "send"
// beside it into "schedule".
//
// THE PALETTE IS THE REMIND PALETTE'S SIBLING, not a second engine: the same
// `RemindTimes.match` answers "send when" exactly as it answers "remind me
// when", typed words and pinned picks alike, and every row states the instant
// it resolves to (`RemindRow`, shared). What differs is what a pick DOES. A
// reminder acts at once; a send time only sets `ComposeState.sendAt`, and the
// mail goes when the sender presses the verb that now reads "schedule".
//
// Hidden entirely unless the daemon keeps an outbox
// (`AppStore.scheduledSendAvailable`): an old daemon would 404 the click.
//
// On the Mac the palette is a window-level overlay mounted by ActionLayer, the
// way `h` mounts the remind palette, so it takes the keys through the same
// modal context. On a phone the composer is already a sheet, so the clock
// presents the palette as a sheet of its own.

import SwiftUI

// MARK: - the clock

struct ScheduleClock: View {
    let slot: DraftSaver.Slot

    @Environment(AppStore.self) private var store

    /// The chord that opens the palette from anywhere in a composer. ⌘⇧L,
    /// "later": ⌘S was the mnemonic, but it is "save" in every Mac hand, and a
    /// save reflex opening a time palette would be a trap. Bound in both
    /// composers' key sets; printed on the clock itself.
    static let chord = "⌘⇧L"
    static let chordKey = "L"

    private var sendAt: Date? {
        (slot == .compose ? store.compose : store.inlineReply)?.sendAt
    }

    var body: some View {
        // A SET TIME STAYS VISIBLE even if the gate goes off under it (a
        // reconnect to an older daemon): the verbs already read "send" then,
        // and the chip's × is the way to clear what no longer applies.
        if store.scheduledSendAvailable || sendAt != nil {
            Group {
                if let sendAt {
                    setChip(sendAt)
                } else if store.scheduledSendAvailable {
                    // JUST THE ICON. The words and the chord are the help
                    // text; the chip a pick turns it into says the rest.
                    Button { store.schedulePicker = slot } label: {
                        Image(systemName: "clock")
                            .font(Typo.ui(13))
                            .foregroundStyle(Palette.inkFaint)
                            .frame(width: Space.scaled(22), height: Space.scaled(22))
                            .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                    .pointingHand()
                    .help(Self.help)
                    .accessibilityLabel(Self.help)
                }
            }
            #if !os(macOS)
                .sheet(isPresented: pickerOpen) {
                    SchedulePalette(slot: slot) { store.schedulePicker = nil }
                }
            #endif
        }
    }

    /// THE CHIP A PICK TURNS THE CLOCK INTO: when, and a way out. The time
    /// reopens the palette (to move it); the × clears it, and the verb goes
    /// back to "send".
    private func setChip(_ sendAt: Date) -> some View {
        HStack(spacing: Space.s4) {
            Button {
                if store.scheduledSendAvailable { store.schedulePicker = slot }
            } label: {
                HStack(spacing: Space.scaled(3)) {
                    Image(systemName: "clock.fill").font(Typo.ui(9, weight: .semibold))
                    // A minute tick: "today 9:00" has to become "yesterday"
                    // or a weekday across midnight while the composer sits
                    // open, and nothing else re-renders the chip.
                    TimelineView(.everyMinute) { context in
                        Text(ScheduleCopy.chip(sendAt, now: context.date))
                    }
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .pointingHand()
            .help(
                store.scheduledSendAvailable
                    ? "goes out \(RemindTimes.detail(sendAt)) · click to change"
                    : "this account cannot schedule mail. it will send now")
            .accessibilityLabel("Scheduled for \(RemindTimes.detail(sendAt)), change")
            Button { store.setSendAt(slot, nil) } label: {
                Image(systemName: "xmark")
                    .font(Typo.ui(8, weight: .bold))
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .pointingHand()
            .help("send now instead")
            .accessibilityLabel("Clear the send time")
        }
        .modifier(ClockCapsule(on: true))
    }

    /// The icon's help text and accessibility label: "send later ⌘⇧L" on
    /// the Mac, where the chord is how anyone learns it; the words alone on
    /// a phone, which has no chord.
    static var help: String {
        #if os(macOS)
            "\(ScheduleCopy.clockHelp) \(chord)"
        #else
            ScheduleCopy.clockHelp
        #endif
    }

    #if !os(macOS)
        private var pickerOpen: Binding<Bool> {
            Binding(
                get: { store.schedulePicker == slot },
                set: { if !$0, store.schedulePicker == slot { store.schedulePicker = nil } })
        }
    #endif
}

/// `Chip`'s capsule, as a modifier so the set chip can hold two buttons.
private struct ClockCapsule: ViewModifier {
    let on: Bool

    func body(content: Content) -> some View {
        let tone = on ? Palette.accent : Palette.inkFaint
        content
            .font(Typo.micro)
            .foregroundStyle(tone)
            .padding(.horizontal, Space.scaled(7))
            .padding(.vertical, Space.scaled(2.5))
            .background(Capsule().fill(on ? tone.opacity(0.16) : Color.clear))
            .overlay(Capsule().strokeBorder(tone.opacity(on ? 0.28 : 0.22), lineWidth: 0.75))
            .fixedSize()
    }
}

// MARK: - the palette

struct SchedulePalette: View {
    let slot: DraftSaver.Slot
    let onClose: () -> Void

    @Environment(AppStore.self) private var store

    @State private var query = ""
    @State private var selection = 0
    /// Pinned when the palette opens, for the remind palette's reason: one
    /// clock for every row, so the list cannot re-rank under the cursor.
    @State private var opened = Date()
    @State private var hits: [RemindHit] = []
    @FocusState private var focused: Bool

    private var composer: ComposeState? {
        slot == .compose ? store.compose : store.inlineReply
    }

    var body: some View {
        surface
            .keyContext(.modal)
            .keyBindings(.modal, bindings)
            .onAppear {
                focused = true
                refreshHits()
            }
            // The composer it was setting a time FOR went away underneath it
            // (a thread closed, an account switched): a pick would land on
            // nothing, so the palette goes too.
            .onChange(of: composer == nil) { _, gone in
                if gone { onClose() }
            }
    }

    private func refreshHits() {
        hits = RemindTimes.match(query, now: opened)
        selection = max(0, min(selection, max(0, hits.count - 1)))
    }

    private var surface: some View {
        PaletteShell(
            glassID: "schedule", icon: "clock",
            title: composer?.sendAt == nil ? "Send later" : "Move the send time",
            // What it is set to now, when this is a move: the one fact the
            // list itself cannot show.
            current: composer?.sendAt.map { "now \(ScheduleCopy.chip($0))" },
            isEmpty: hits.isEmpty,
            emptyText:
                "no time in “\(query)”. try a day (\"friday\", \"aug 30\"), a wait (\"in 3 hours\") or leave it empty for the usual ones.",
            selection: selection, idAt: { hits[safe: $0]?.id },
            verb: "set",
            // The half of this that is not obvious: the daemon sends it, so a
            // closed laptop does not stop it.
            note: "goes out then, even with Passband closed",
            onClose: onClose
        ) {
            TextField("send when...", text: $query)
                .focused($focused)
                .onChange(of: query) { _, _ in
                    selection = 0
                    refreshHits()
                }
        } rows: {
            ForEach(Array(hits.enumerated()), id: \.element.id) { i, hit in
                RemindRow(
                    hit: hit, selected: i == selection,
                    onHover: { selection = i },
                    onPick: { apply(hit) })
                .id(hit.id)
            }
        }
        #if !os(macOS)
            .presentationDragIndicator(.visible)
        #endif
    }

    /// allowInInput for the remind palette's reason: the field has focus, and
    /// without it Escape would fall through to the composer underneath.
    ///
    /// ⌘↵ AND ⌘⇧↵ ARE THE PALETTE'S. Both composers bind them to send, and
    /// the composer's set sits under this one in the same context (the pane)
    /// or under a context this one covers (the inline reply): left unbound
    /// here, a ⌘↵ typed into this field would send the mail from under the
    /// palette before a time was picked. ⌘↵ sets the pick like ↵ does; the
    /// override is swallowed, because consent to the guard is never given
    /// from a time picker.
    private var bindings: [KeyBinding] {
        [
            KeyBinding("Escape", "cancel", allowInInput: true) { onClose() },
            KeyBinding("ArrowDown", "next", allowInInput: true) {
                selection = min(hits.count - 1, selection + 1)
            },
            KeyBinding("ArrowUp", "prev", allowInInput: true) {
                selection = max(0, selection - 1)
            },
            KeyBinding("Enter", "set send time", allowInInput: true) { applySelection() },
            KeyBinding("Enter", "set send time", meta: true, allowInInput: true) {
                applySelection()
            },
            KeyBinding("shift+Enter", "set send time", meta: true, allowInInput: true) {},
        ]
    }

    private func applySelection() {
        if let hit = hits[safe: selection] { apply(hit) }
    }

    /// Set the time and close. Nothing is sent: the composer's verb now reads
    /// "schedule", and pressing it is the act.
    private func apply(_ hit: RemindHit) {
        // THE PINNED CLOCK'S ESCAPE HATCH, as in the remind palette: a row
        // that went by while the palette sat open is re-resolved against a
        // fresh clock rather than set in the past.
        guard hit.date > Date() else {
            opened = Date()
            selection = 0
            refreshHits()
            return
        }
        store.setSendAt(slot, hit.date)
        onClose()
    }
}
