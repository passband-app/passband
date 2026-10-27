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
    @Namespace private var paletteGlass
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

    @ViewBuilder
    private var surface: some View {
        #if os(macOS)
            OverlayScrim(alignment: .top, topInset: 210, onDismiss: onClose) {
                GlassEffectContainer(spacing: 8) {
                    palette
                        .frame(width: Typo.measure(560))
                        .passbandGlass(
                            .pane, cornerRadius: Radius.modal, tint: Palette.glassTintStrong,
                            id: "schedule", in: paletteGlass)
                        .shadow(color: .black.opacity(0.32), radius: 46, y: 20)
                }
            }
        #else
            palette
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
                .background(Palette.canvas.ignoresSafeArea())
                .presentationDetents([.large])
                .presentationDragIndicator(.visible)
        #endif
    }

    private var palette: some View {
        VStack(alignment: .leading, spacing: 0) {
            header
            input
            Divider().overlay(Palette.hairline)
            list
            footer
        }
    }

    private var header: some View {
        HStack(spacing: Space.s8) {
            Image(systemName: "clock")
                .font(Typo.ui(12, weight: .semibold))
                .foregroundStyle(Palette.accent)
            Text(composer?.sendAt == nil ? "Send later" : "Move the send time")
                .font(Typo.ui(13, weight: .semibold))
                .foregroundStyle(Palette.ink)
            Spacer(minLength: Space.s4)
            // What it is set to now, when this is a move: the one fact the
            // list itself cannot show.
            if let current = composer?.sendAt {
                Text("now \(ScheduleCopy.chip(current))")
                    .font(Typo.micro)
                    .foregroundStyle(Palette.inkDim)
                    .fixedSize()
            }
        }
        .padding(.horizontal, Space.s16)
        .padding(.top, Space.s14)
        .padding(.bottom, Space.s8)
    }

    private var input: some View {
        TextField("send when...", text: $query)
            .textFieldStyle(.plain)
            .font(Typo.ui(15))
            .foregroundStyle(Palette.ink)
            .focused($focused)
            .autocorrectionDisabled()
            .padding(.horizontal, Space.s16)
            .padding(.top, Space.s4)
            .padding(.bottom, Space.s12)
            .onChange(of: query) { _, _ in
                selection = 0
                refreshHits()
            }
    }

    private var list: some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(spacing: 1) {
                    if hits.isEmpty {
                        Text(
                            "no time in “\(query)”. try a day (\"friday\", \"aug 30\"), a wait (\"in 3 hours\") or leave it empty for the usual ones."
                        )
                        .font(Typo.micro)
                        .foregroundStyle(Palette.inkFaintest)
                        .fixedSize(horizontal: false, vertical: true)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(Space.s14)
                    } else {
                        ForEach(Array(hits.enumerated()), id: \.element.id) { i, hit in
                            RemindRow(
                                hit: hit, selected: i == selection,
                                onHover: { selection = i },
                                onPick: { apply(hit) })
                            .id(hit.id)
                        }
                    }
                }
                .padding(.horizontal, Space.s8)
                .padding(.vertical, Space.s6)
            }
            .frame(maxHeight: Space.scaled(230))
            .onChange(of: selection) { _, i in
                guard let hit = hits[safe: i] else { return }
                withAnimation(.easeOut(duration: 0.1)) { proxy.scrollTo(hit.id, anchor: .center) }
            }
        }
    }

    private var footer: some View {
        HStack(spacing: Space.s4) {
            #if os(macOS)
                Kbd("↑"); Kbd("↓")
                Text("pick").font(Typo.micro).foregroundStyle(Palette.inkFaintest)
                Text("·").foregroundStyle(Palette.inkFaintest)
                Kbd("↵")
                Text("set").font(Typo.micro).foregroundStyle(Palette.inkFaintest)
                Text("·").foregroundStyle(Palette.inkFaintest)
                Kbd("esc")
                Text("cancel").font(Typo.micro).foregroundStyle(Palette.inkFaintest)
            #else
                Text("tap to set").font(Typo.micro).foregroundStyle(Palette.inkFaintest)
            #endif
            Spacer()
            // The half of this that is not obvious: the daemon sends it, so a
            // closed laptop does not stop it.
            Text("goes out then, even with Passband closed")
                .font(Typo.micro)
                .foregroundStyle(Palette.accent.opacity(0.8))
        }
        .padding(.horizontal, Space.s16)
        .padding(.vertical, Space.s10)
        .overlay(alignment: .top) { Hairline() }
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
