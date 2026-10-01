// The global action surface, above every other layer: undo/notice toasts, the
// unsubscribe-violation prompt, rule editor, process mode, the 2FA modal,
// triage-fix palette, ⌘K ask bar, shortcuts overlay, the re-triage confirm and
// the re-triage run itself. The composer is NOT here anymore — it is a layout
// pane (see ComposePane in MainShell), not an overlay.
// Read views own the list keymap; this layer extends that same "list" context
// with t and p. Modal-context keys live inside each overlay.

import SwiftUI

struct ActionLayer: View {
    @Environment(AppStore.self) private var store

    @State private var blockPrompt: UnsubscribeRecord?
    @State private var blockBusy = false
    /// Senders acted on this session, so a lagging read model can't re-nag.
    @State private var acted: Set<String> = []

    var body: some View {
        ZStack(alignment: .bottomLeading) {
            // FIRST, so everything else in this layer stacks above it: the
            // undo chip the tour teaches has to be visible over the tour's own
            // scrim, and the rule editor it opens has to cover its card.
            if store.tour.active { TourOverlay() }

            // Toast stack, bottom-left above every fullscreen surface. The
            // Spacers + fixedSize are load-bearing: a GlassEffectContainer
            // expands to the size it is offered, so an unconstrained one here
            // is an invisible full-window pane that eats every scroll and click.
            VStack(spacing: 0) {
                Spacer(minLength: 0)
                HStack(spacing: 0) {
                    toastStack.fixedSize()
                    Spacer(minLength: 0)
                }
            }
            .padding(.leading, SidebarRail.railWidth + Space.s14)
            .padding(.bottom, Space.scaled(18))

            // AUTOMATIC UNSUBSCRIBES, bottom-right of the sitrep: the toasts'
            // opposite corner, so a job and an undo chip never fight for one.
            // Sitrep only, with no reader or composer over it: `u` leaves the
            // reader on purpose, and this is where the user lands.
            if showsUnsubscribeTray {
                VStack(spacing: 0) {
                    Spacer(minLength: 0)
                    HStack(spacing: 0) {
                        Spacer(minLength: 0)
                        UnsubscribeTray().fixedSize()
                    }
                }
                .padding(.trailing, Space.scaled(18))
                .padding(.bottom, Space.scaled(18))
                .transition(.opacity)
            }

            if let request = store.ruleEditor {
                // Keyed by the request: a click on an in-effect rule swaps the
                // request under an open card, and without a fresh identity the
                // edit card would inherit the tune card's @State.
                RuleEditor(
                    request: request,
                    draft: request.draftKey.flatMap { store.ruleDrafts[$0] }
                ) { store.closeRuleEditor() }
                .id(request.id)
            }
            if let request = store.groupEditor {
                GroupEditor(request: request) { store.closeGroupEditor() }
            }
            if store.processModeOpen { ProcessMode { store.processModeOpen = false } }
            if !store.authQueue.isEmpty { AuthCodeModal() }
            if let target = store.triageFix {
                TriageFixPalette(target: target) { store.closeTriageFix() }
            }
            if let target = store.remindTarget {
                RemindPalette(target: target) { store.closeRemind() }
            }
            // The composers' send-later clock. Here rather than on the
            // composer for the reason the remind palette is: a window-level
            // overlay takes the keys through the modal context above the
            // composer's own, and the pane is too narrow to hold it.
            if let slot = store.schedulePicker {
                SchedulePalette(slot: slot) { store.schedulePicker = nil }
            }
            if store.askBarOpen { AskBar { store.askBarOpen = false } }
            if store.shortcutsOpen { ShortcutsOverlay { store.shortcutsOpen = false } }
            // Above everything else in this layer, because it is shown at most
            // once per version at a moment nothing else has been raised yet, and
            // a modal that can end up UNDER something has to be dismissed twice
            // to be read once.
            if store.whatsNew.active { WhatsNewCard() }
            // The question that leads to the modal below, and it sits directly
            // under it for the same reason that one is last: what it is asking
            // is whether to take the app away, and that cannot be answered from
            // behind whatever else happened to be open.
            if let days = store.retriageAsk { RetriageConfirm(days: days) }
            if let ask = store.reconnectAsk { ReconnectAccountPrompt(ask: ask) }
            // LAST, and it outranks even the card above: that one is a greeting
            // the user can dismiss, this one is the app being unavailable while
            // a re-triage rewrites what every other overlay here is about.
            // Anything already open when it starts stays open UNDER it and is
            // there again when the run ends — closing them would be this modal
            // reaching outside itself to tidy up state it does not own.
            if let run = store.retriage { RetriageModal(run: run) }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .bottomLeading)
        .keyBindings(.list, [
            KeyBinding("t", "tune sender") {
                if let u = store.selectedUpdate { Actions.tune(sender: u.sender) }
            },
            KeyBinding("p", "process mode") { store.processModeOpen = true },
            // `c` = compose, registered per-SURFACE (here for the lists, and again
            // in the sitrep and the reader) rather than globally: a global one would
            // fire from inside a modal, where the letter belongs to whatever is
            // being typed.
            KeyBinding("c", "new message") { store.openComposeNew() },
        ])
        // The scan rides the poll tick from a leaf of its own: this layer is
        // every toast, undo chip and modal in the app, and reading the tick
        // here re-evaluated all of it every ten seconds.
        .background { ViolationScanKeeper(scan: scanViolations) }
    }

    private var showsUnsubscribeTray: Bool {
        store.activeView == .sitrep && store.threadId == nil && !store.composeOpen
            && !UnsubscribeJobs.shared.items.isEmpty
    }

    // MARK: - toasts

    private var toastStack: some View {
        GlassEffectContainer(spacing: 10) {
            VStack(alignment: .leading, spacing: Space.scaled(7)) {
                // Sender strings are email-derived: rendered as Text only,
                // never as markup.
                if let prompt = blockPrompt {
                    violationPrompt(prompt)
                }
                ForEach(store.toasts) { toast in
                    Button {
                        store.dismissToast(toast.id)
                    } label: {
                        Text(toast.text)
                            .font(Typo.rowSub)
                            .foregroundStyle(tone(toast.tone))
                            .padding(.horizontal, Space.s12)
                            .padding(.vertical, Space.scaled(7))
                    }
                    .buttonStyle(.plain)
                    .background(
                        RoundedRectangle(cornerRadius: 11, style: .continuous)
                            .fill(.clear)
                    )
                    .glassEffect(
                        .regular.tint(tone(toast.tone).opacity(0.16)).interactive(),
                        in: .rect(cornerRadius: 11, style: .continuous)
                    )
                    .overlay(alignment: .leading) {
                        RoundedRectangle(cornerRadius: 1)
                            .fill(tone(toast.tone))
                            .frame(width: 2)
                            .padding(.vertical, Space.s6)
                    }
                    .transition(.move(edge: .leading).combined(with: .opacity))
                }
                ForEach(store.undos) { undo in
                    Button {
                        Task { await store.fireUndo(undo.id) }
                    } label: {
                        HStack(spacing: Space.s14) {
                            Text(undo.label)
                                .font(Typo.rowSub)
                                .foregroundStyle(Palette.ink)
                            HStack(spacing: Space.s4) {
                                Kbd("u")
                                Text("undo")
                                    .font(Typo.micro)
                                    .foregroundStyle(Palette.inkFaintest)
                            }
                        }
                        .padding(.horizontal, Space.s12)
                        .padding(.vertical, Space.scaled(7))
                    }
                    .buttonStyle(.plain)
                    .glassEffect(
                        .regular.tint(Palette.warn.opacity(0.18)).interactive(),
                        in: .rect(cornerRadius: 11, style: .continuous)
                    )
                    .overlay(alignment: .leading) {
                        RoundedRectangle(cornerRadius: 1)
                            .fill(Palette.warn)
                            .frame(width: 2)
                            .padding(.vertical, Space.s6)
                    }
                    .transition(.move(edge: .leading).combined(with: .opacity))
                    .tourTarget(.undoToast)
                }
            }
        }
        .animation(.smooth(duration: 0.24), value: store.toasts.count)
        .animation(.smooth(duration: 0.24), value: store.undos.count)
    }

    private func tone(_ tone: Toast.Tone) -> Color {
        switch tone {
        case .error: Palette.danger
        case .success: Palette.positive
        case .info: Palette.inkDim
        }
    }

    // MARK: - unsubscribe violations

    private func violationPrompt(_ record: UnsubscribeRecord) -> some View {
        VStack(alignment: .leading, spacing: Space.scaled(9)) {
            Text(
                "\(SenderCache.resolved(record.sender).displayName) is ignoring your unsubscribe — "
                    + "\(record.violation_count) email\(record.violation_count == 1 ? "" : "s") "
                    + "since you asked \(Fmt.relAge(record.requested_at)) ago."
            )
            .font(Typo.rowSub)
            .foregroundStyle(Palette.ink)
            .fixedSize(horizontal: false, vertical: true)
            .frame(maxWidth: Space.scaled(360), alignment: .leading)

            HStack(spacing: Space.s8) {
                Button("Block sender") { Task { await block(record) } }
                    .buttonStyle(.glassProminent)
                    .tint(Palette.danger)
                    .disabled(blockBusy)
                Button("Keep") { Task { await keep(record) } }
                    .buttonStyle(.glass)
                    .disabled(blockBusy)
            }
            .controlSize(.small)
        }
        .padding(Space.scaled(13))
        .passbandGlass(.pane, cornerRadius: 14, tint: Palette.dangerSoft)
        .transition(.move(edge: .leading).combined(with: .opacity))
    }

    private func scanViolations() async {
        let gen = UnsubscribeJobs.shared.generation
        guard let rows = try? await APIClient.shared.getUnsubscribes() else { return }
        // The same rows carry automatic-unsubscribe jobs: a `needs_you` left
        // from before a relaunch reaches the tray through this fetch.
        UnsubscribeJobs.shared.absorb(rows, generation: gen)
        // Surface one at a time: a stack of these is a nag, not a prompt.
        let next = rows.first {
            $0.violation_count > 0 && $0.resolution == nil && !acted.contains($0.sender)
        }
        // Assignment is invalidation, and the answer is nil nearly always.
        if next != blockPrompt { blockPrompt = next }
    }

    private func block(_ record: UnsubscribeRecord) async {
        guard !blockBusy else { return }
        blockBusy = true
        // Mark acted immediately so a refresh mid-flight can't re-surface it.
        acted.insert(record.sender)
        do {
            // Exact sender address, not the domain.
            try await Actions.createBlockRule(sender: record.sender)
            // No message on screen to name, but the rule is still a verdict on
            // the sender, so the same rows clear here as anywhere else.
            store.noteSenderResolved(record.sender)
            try await APIClient.shared.setUnsubResolution(
                sender: record.sender, resolution: .blocked)
            store.pushToast("blocked \(record.sender)", .success)
        } catch {
            store.pushToast(errText(error, "block failed"), .error)
        }
        blockBusy = false
        blockPrompt = nil
    }

    private func keep(_ record: UnsubscribeRecord) async {
        guard !blockBusy else { return }
        blockBusy = true
        acted.insert(record.sender)
        // Best-effort: the session-level `acted` set already suppresses re-nag.
        _ = try? await APIClient.shared.setUnsubResolution(
            sender: record.sender, resolution: .dismissed)
        blockBusy = false
        blockPrompt = nil
    }
}

// MARK: - shortcuts overlay

/// Keyboard-shortcuts help ('?'). Curated and grouped by hand rather than
/// dumped from the keymap registry, which carries per-context duplicates.
struct ShortcutsOverlay: View {
    let onClose: () -> Void

    private struct Group: Identifiable {
        var title: String
        var items: [(keys: [String], desc: String)]
        var id: String { title }
    }

    private static let groups: [Group] = [
        Group(
            title: "Sitrep & inbox",
            items: [
                (["j", "k"], "move selection"),
                (["Enter"], "open thread"),
                (["r"], "reply"),
                (["c"], "new message"),
                (["e", "d"], "mark done"),
                (["v"], "fix a wrong triage verdict"),
                (["h"], "remind me about this later"),
                (["t"], "tune sender rule"),
                (["f"], "search this sender"),
                (["p"], "process mode"),
                (["u"], "undo last action"),
            ]),
        Group(
            title: "Thread viewer",
            items: [
                (["j", "k"], "older / newer message"),
                (["←", "→"], "previous / next queued email"),
                (["r"], "reply"),
                (["Enter"], "reply all"),
                (["c"], "new message"),
                (["e", "d"], "done, and close the reader"),
                (["E", "D"], "done, and open the next email"),
                (["h"], "remind + next"),
                (["u"], "unsubscribe from this sender"),
                (["t"], "new rule for this sender"),
                // `f` is FORWARD in the reader and search-this-sender
                // everywhere else; the group above still shows `f` as search,
                // and this row is the one place a reader can see that the key
                // changes hands. The reader has no search-this-sender key of
                // its own: `/` then `from:` is the spelled-out way.
                (["f"], "forward this message"),
                (["Esc"], "back"),
            ]),
        Group(
            title: "Navigate",
            items: [
                (["1", "2", "3", "4", "5"], "sitrep · emails · auth · rules · groups"),
                (["⌘["], "back"),
                (["⌘]"], "forward"),
                (["a"], "browse all mail"),
                (["n"], "noise page (and back)"),
                (["/"], "search"),
                (["⌘K"], "ask your inbox"),
                (["⌘⇧K"], "new ask chat"),
                (["⌘,"], "settings"),
                (["g"], "auth messages"),
                (["T"], "rules"),
                (["A"], "audit log (in settings)"),
            ]),
        Group(
            title: "App",
            items: [
                (["\\"], "toggle light / dark theme"),
                (["?"], "this help"),
                (["Esc"], "close panel / overlay"),
            ]),
    ]

    var body: some View {
        OverlayScrim(onDismiss: onClose) {
            ModalCard(width: 620) {
                Text("Keyboard shortcuts")
                    .font(Typo.serif(22, weight: .medium))
                    .foregroundStyle(Palette.ink)

                LazyVGrid(
                    columns: [GridItem(.flexible(), spacing: Space.scaled(22)), GridItem(.flexible())],
                    alignment: .leading, spacing: Space.scaled(18)
                ) {
                    ForEach(Self.groups) { group in
                        VStack(alignment: .leading, spacing: Space.scaled(5)) {
                            Text(group.title)
                                .font(Typo.sectionLabel)
                                .foregroundStyle(Palette.accent)
                                .textCase(.uppercase)
                                .padding(.bottom, Space.s2)
                            ForEach(group.items, id: \.desc) { item in
                                HStack(spacing: Space.s6) {
                                    Text(item.desc)
                                        .font(Typo.micro)
                                        .foregroundStyle(Palette.inkDim)
                                    Spacer(minLength: Space.s8)
                                    ForEach(item.keys, id: \.self) { Kbd($0) }
                                }
                            }
                        }
                    }
                }

                HStack(spacing: Space.s4) {
                    Spacer()
                    Kbd("?")
                    Text("or").font(Typo.micro).foregroundStyle(Palette.inkFaintest)
                    Kbd("Esc")
                    Text("to close").font(Typo.micro).foregroundStyle(Palette.inkFaintest)
                }
            }
        }
        .keyContext(.modal)
        .keyBindings(.modal, [
            KeyBinding("Escape", "close help") { onClose() },
            KeyBinding("?", "close help") { onClose() },
        ])
    }
}


/// Asks for the unsubscribe ledger on the poll tick, from a zero-size leaf
/// (see the call site), and NOT ON EVERY TICK: a violation is something a
/// sender did days ago, and a request every ten seconds for it was the
/// busiest thing this client asked the daemon for. Once a minute.
private struct ViolationScanKeeper: View {
    @Environment(AppStore.self) private var store
    let scan: () async -> Void
    @State private var lastScan: Date?
    private static let interval: TimeInterval = 60

    var body: some View {
        Color.clear
            .frame(width: 0, height: 0)
            .task(id: store.lastRefresh) {
                if let lastScan, Date().timeIntervalSince(lastScan) < Self.interval { return }
                lastScan = Date()
                await scan()
            }
    }
}
