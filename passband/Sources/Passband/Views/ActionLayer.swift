// The global action surface, above every other layer: undo/notice toasts, the
// unsubscribe-violation prompt, rule editor, the 2FA modal,
// triage-fix palette, ⌘K ask bar, shortcuts overlay, the re-triage confirm and
// the re-triage run itself. The composer is NOT here anymore — it is a layout
// pane (see ComposePane in MainShell), not an overlay.
// Read views own the list keymap; this layer extends that same "list" context
// with c. Modal-context keys live inside each overlay.

import SwiftUI

struct ActionLayer: View {
    @Environment(AppStore.self) private var store

    @State private var blockPrompt: UnsubscribeRecord?
    @State private var blockBusy = false
    /// Senders acted on this session, so a lagging read model can't re-nag.
    @State private var acted: Set<String> = []
    /// The smart block proposed for the sender on the notice, once it lands.
    @State private var smartBlocks: [String: SmartBlockProposal] = [:]
    /// Senders already asked about this session, answer or not: the scan
    /// re-runs every minute and one model call per notice is the budget.
    @State private var smartAsked: Set<String> = []

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
                                if let expiresAt = undo.expiresAt {
                                    UndoCountdown(
                                        startedAt: undo.createdAt, expiresAt: expiresAt
                                    )
                                    .padding(.leading, 3)
                                }
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
        let name = SenderCache.resolved(record.sender).displayName
        let smart = smartBlocks[record.sender]
        return VStack(alignment: .leading, spacing: Space.scaled(9)) {
            Text(
                "\(name) is ignoring your unsubscribe. "
                    + "\(record.violation_count) email\(record.violation_count == 1 ? "" : "s") "
                    + "since you asked \(Fmt.relAge(record.requested_at)) ago."
            )
            .font(Typo.rowSub)
            .foregroundStyle(Palette.ink)
            .fixedSize(horizontal: false, vertical: true)

            if let smart { smartBlockProposal(smart) }

            HStack(spacing: Space.s8) {
                // With a smart block on offer it is the recommendation, and one
                // notice gets one prominent button.
                if smart == nil {
                    Button("Block sender") { Task { await block(record) } }
                        .buttonStyle(.glassProminent)
                        .tint(Palette.danger)
                } else {
                    Button("Block sender") { Task { await block(record) } }
                        .buttonStyle(.glass)
                }
                if let smart {
                    Button("Smart block") { Task { await smartBlock(record, smart) } }
                        .buttonStyle(.glassProminent)
                        .tint(Palette.accent)
                        .help("Saves this rule for \(record.sender): \(smart.rule)")
                        .transition(.opacity)
                }
                Button("Keep") { Task { await keep(record) } }
                    .buttonStyle(.glass)
            }
            .controlSize(.small)
            .disabled(blockBusy)
        }
        // A WIDTH, not a maximum. The toast stack is measured at its ideal
        // size, where a maximum proposes nothing and the sentence measures as
        // one line; it then wraps to two when placed, and the line the stack
        // never counted hung down through the layer's bottom margin.
        .frame(width: Typo.measure(360), alignment: .leading)
        .padding(Space.scaled(13))
        .passbandGlass(.pane, cornerRadius: 14, tint: Palette.dangerSoft)
        .transition(.move(edge: .leading).combined(with: .opacity))
        .animation(.smooth(duration: 0.24), value: smart)
        .task(id: record.sender) { await proposeSmartBlock(for: record.sender) }
    }

    /// What the smart block would do, and under it the sentence it saves: the
    /// rule is model text about the sender's own subjects, so it is on screen
    /// in full before the button that keeps it can be pressed.
    private func smartBlockProposal(_ smart: SmartBlockProposal) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: Space.s8) {
            Image(systemName: "sparkles")
                .font(Typo.micro)
                .foregroundStyle(Palette.accent)
            VStack(alignment: .leading, spacing: Space.s2) {
                Text("Keep \(smart.keeps), drop \(smart.drops)")
                    .font(Typo.rowSub)
                    .foregroundStyle(Palette.ink)
                Text("Rule: \(smart.rule)")
                    .font(Typo.micro)
                    .foregroundStyle(Palette.inkDim)
            }
            .fixedSize(horizontal: false, vertical: true)
        }
        .transition(.opacity)
    }

    /// One ask per sender per session. Silence on every failure: no model, an
    /// older daemon, or a sender with nothing worth keeping all leave the
    /// notice as the two buttons it always was.
    private func proposeSmartBlock(for sender: String) async {
        guard !smartAsked.contains(sender) else { return }
        smartAsked.insert(sender)
        guard let proposal = try? await APIClient.shared.suggestSmartBlock(sender: sender)
        else { return }
        smartBlocks[sender] = proposal
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

    private func smartBlock(_ record: UnsubscribeRecord, _ smart: SmartBlockProposal) async {
        guard !blockBusy else { return }
        blockBusy = true
        acted.insert(record.sender)
        do {
            // Exact sender address, like the block. Forward-only: nothing on
            // the board moves, so there is no sender to note as resolved.
            try await Actions.createSmartBlockRule(sender: record.sender, want: smart.rule)
            try await APIClient.shared.setUnsubResolution(
                sender: record.sender, resolution: .filtered)
            store.pushToast("filtering \(record.sender), keeping \(smart.keeps)", .success)
        } catch {
            store.pushToast(errText(error, "smart block failed"), .error)
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

/// Keyboard-shortcuts help ('?'). Draws `ShortcutCatalog`, the one table the
/// assistant's list_shortcuts tool also reads, so the two cannot disagree.
struct ShortcutsOverlay: View {
    @Environment(AppStore.self) private var store
    let onClose: () -> Void

    /// The sheet's way out to the assistant. It CLOSES THE SHEET FIRST: the
    /// ask bar is mounted beneath this overlay, so the global ⌘K alone would
    /// open it underneath, behind a scrim, where the hint that sent somebody
    /// there had just hidden it.
    private func askAgent() {
        onClose()
        store.askBarOpen = true
    }

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
                    ForEach(ShortcutCatalog.groups) { group in
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
                    // A list of keys answers "which key", never "how do I". The
                    // assistant reads this same table and the rest of the guide.
                    Button(action: askAgent) {
                        HStack(spacing: Space.s4) {
                            Text("you can always ask the agent!")
                                .font(Typo.micro)
                                .foregroundStyle(Palette.inkDim)
                            Kbd("⌘K")
                        }
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
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
            // Ahead of the global ⌘K, which this context would otherwise let
            // through to open the bar under the sheet (see `askAgent`).
            KeyBinding("k", "ask your inbox", meta: true, allowInInput: true) { askAgent() },
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

/// How long an undo chip has left: a small ring that drains to nothing as the
/// window closes. Drawn from the clock rather than animated to a value, like
/// the reader's hold ring, so it empties on the same second the chip leaves
/// even when it mounts late or the layer re-renders mid-sweep.
private struct UndoCountdown: View {
    let startedAt: Date
    let expiresAt: Date

    private static let size: CGFloat = 11

    var body: some View {
        TimelineView(.animation(minimumInterval: 1 / 30)) { ctx in
            let total = max(0.001, expiresAt.timeIntervalSince(startedAt))
            let left = min(1, max(0, expiresAt.timeIntervalSince(ctx.date) / total))
            ZStack {
                Circle()
                    .stroke(Palette.hairlineStrong, lineWidth: 1.75)
                Circle()
                    .trim(from: 0, to: left)
                    .stroke(Palette.warn, style: StrokeStyle(lineWidth: 1.75, lineCap: .round))
                    .rotationEffect(.degrees(-90))
            }
            .frame(width: Self.size, height: Self.size)
        }
        .accessibilityHidden(true)
    }
}
