// THE QUICK CLEAR SPRINT, drawn: one card over the board holding exactly the
// queue the banner promised (Model/QuickClearSprint.swift).
//
// ONE ITEM AT A TIME, because the point is to blow through it with the
// keyboard: the item on screen is the only thing a key can mean, so `e` never
// has to ask "which one". The next two wait underneath, faint, so the queue
// reads as short. The card is editorial-roomy (`Space.s32`, like What's New
// and the guide's boundary card), with the item in a well like a Reading card.
//
// THE CLOCK IS GENTLE ON PURPOSE. It counts up against the budget on a thin
// accent track that never turns red, and past the budget it says so once and
// then stops counting at the owner. No confetti at the end: one warm line.
//
// Keys, all in the modal context so nothing on the board underneath hears
// them: e/d done, Enter open, h later, → or Space not now, ← back, u undo,
// Esc stop. The reader opened from here hides the card (`quickClearUp`) and
// the card is back, where it was, when the reader closes.

#if os(macOS)
    import SwiftUI

    struct QuickClearView: View {
        @Environment(AppStore.self) private var store
        let sprint: QuickClearSprint

        private enum Phase {
            case smart
            case item(QuickClearSignal.Item)
            case summary
        }

        /// Asked fresh at key time as well as in `body`, so a key pressed in
        /// the instant after a done never acts on the item that just left.
        private var phase: Phase {
            if sprint.ended { return .summary }
            if sprint.onSmartStep { return .smart }
            if let first = sprint.pending.first { return .item(first) }
            return .summary
        }

        private var pendingIds: [Int] { sprint.pending.map(\.message_id) }

        var body: some View {
            // A STRAY CLICK NEVER ENDS IT: the dim is not a way out, Esc is.
            OverlayScrim(onDismiss: {}) {
                ModalCard(width: 560, padding: Space.s32) {
                    VStack(alignment: .leading, spacing: Space.s20) {
                        header
                        switch phase {
                        case .smart: smartCard
                        case .item(let item): itemStage(item)
                        case .summary: summary
                        }
                    }
                }
            }
            .keyContext(.modal)
            .keyBindings(.modal, bindings)
            .onChange(of: pendingIds, initial: true) { _, ids in
                if ids.isEmpty, !sprint.onSmartStep { sprint.noteEmptied() }
            }
            .onChange(of: sprint.onSmartStep) { _, onSmart in
                if !onSmart, pendingIds.isEmpty { sprint.noteEmptied() }
            }
            // The end card once the budget is spent. Re-armed when the card
            // comes back from the reader, which is how a budget that ran out
            // while an email was open shows on the way back.
            .task(id: sprint.keptGoing) {
                guard !sprint.keptGoing else { return }
                let left = Double(sprint.budget) - Date().timeIntervalSince(sprint.startedAt)
                if left > 0 { try? await Task.sleep(for: .seconds(left)) }
                guard !Task.isCancelled else { return }
                sprint.noteTimeUp()
            }
        }

        // MARK: - header

        private var header: some View {
            VStack(alignment: .leading, spacing: Space.s10) {
                HStack(alignment: .firstTextBaseline) {
                    Text("Quick clear")
                        .font(Typo.serif(22, weight: .medium))
                        .foregroundStyle(Palette.ink)
                    Spacer(minLength: Space.s12)
                    if case .item = phase {
                        Text("\(QuickClearQueue.position(total: sprint.total, pending: pendingIds.count)) of \(sprint.total)")
                            .font(Typo.num(12, weight: .medium))
                            .foregroundStyle(Palette.inkDim)
                            .contentTransition(.numericText())
                            .animation(.smooth(duration: 0.2), value: pendingIds.count)
                    }
                }
                if !sprint.ended {
                    ClockTrack(startedAt: sprint.startedAt, budget: sprint.budget)
                }
            }
        }

        // MARK: - the smart clear step

        private var smartCard: some View {
            let working = sprint.smartStep == .working
            return VStack(alignment: .leading, spacing: Space.s16) {
                VStack(alignment: .leading, spacing: Space.s12) {
                HStack(alignment: .firstTextBaseline, spacing: Space.s10) {
                    Image(systemName: "checkmark.rectangle.stack")
                        .font(Typo.ui(15, weight: .medium))
                        .foregroundStyle(Palette.accent)
                    Text("Clear \(EventBanner.smartClearPhrase(sprint.signal.smartClears))")
                        .font(Typo.serif(20))
                        .foregroundStyle(Palette.ink)
                        .fixedSize(horizontal: false, vertical: true)
                }
                Text("Smart clear marks them done in one press. You can undo it.")
                    .font(Typo.rowSub)
                    .foregroundStyle(Palette.inkDim)
                ModalButtons {
                    Button("Not now") { sprint.passSmartClear() }
                        .buttonStyle(.glass)
                        .disabled(working)
                    Button {
                        Task { await sprint.runSmartClear() }
                    } label: {
                        HStack(spacing: Space.s6) {
                            if working { ProgressView().controlSize(.mini) }
                            Text(working ? "Clearing" : "Clear them")
                        }
                    }
                    .buttonStyle(.glassProminent)
                    .disabled(working)
                }
                .padding(.top, Space.s4)
                }
                .padding(Space.s20)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(Well())
                KeyHints(hints: [("↵", "clear them"), ("→", "not now"), ("Esc", "stop")])
            }
        }

        // MARK: - one item

        private func itemStage(_ item: QuickClearSignal.Item) -> some View {
            let next = Array(sprint.pending.dropFirst().prefix(2))
            return VStack(alignment: .leading, spacing: Space.s16) {
                if case .cleared(let n, _) = sprint.smartStep, sprint.smartUndoOffered {
                    Text("Smart clear took \(n). Press u to undo it.")
                        .font(Typo.rowSub)
                        .foregroundStyle(Palette.inkFaint)
                }
                ItemCard(item: item) { sprint.open(item) }
                    .id(item.message_id)
                    .transition(.asymmetric(
                        insertion: .move(edge: .trailing).combined(with: .opacity),
                        removal: .opacity))
                if !next.isEmpty {
                    VStack(alignment: .leading, spacing: Space.s8) {
                        Text("Up next")
                            .font(Typo.sectionLabel)
                            .foregroundStyle(Palette.inkFaintest)
                            .textCase(.uppercase)
                        ForEach(next, id: \.message_id) { NextRow(item: $0) }
                    }
                    .padding(.horizontal, Space.s4)
                }
                KeyHints(hints: [
                    ("e", "done"), ("↵", "open"), ("h", "later"), ("→", "not now"),
                    ("u", "undo"), ("Esc", "stop"),
                ])
            }
            .animation(.smooth(duration: 0.22), value: item.message_id)
        }

        // MARK: - the end

        private var summary: some View {
            let elapsed = sprint.elapsed(at: sprint.endedAt ?? Date())
            let left = sprint.left
            let timeUp = elapsed >= sprint.budget && left > 0
            return VStack(alignment: .leading, spacing: Space.s10) {
                Text(QuickClearQueue.summary(cleared: sprint.cleared, elapsed: elapsed, left: left))
                    .font(Typo.serif(24, weight: .medium))
                    .foregroundStyle(Palette.ink)
                Text(QuickClearQueue.summaryNote(cleared: sprint.cleared, left: left, timeUp: timeUp))
                    .font(Typo.row)
                    .foregroundStyle(Palette.inkDim)
                    .fixedSize(horizontal: false, vertical: true)
                ModalButtons {
                    if left > 0 {
                        Button("Stop here") { store.closeQuickClear() }
                            .buttonStyle(.glass)
                        Button("Keep going") { sprint.keepGoing() }
                            .buttonStyle(.glassProminent)
                    } else {
                        Button("Done") { store.closeQuickClear() }
                            .buttonStyle(.glassProminent)
                    }
                }
                .padding(.top, Space.s12)
            }
        }

        // MARK: - keys

        /// Every key asks for the phase when pressed and DECLINES where it
        /// means nothing, so the bindings never need re-registering.
        private var bindings: [KeyBinding] {
            func onItem(_ act: @escaping (QuickClearSignal.Item) -> Void) -> () -> Bool {
                {
                    guard case .item(let item) = phase else { return false }
                    act(item)
                    return true
                }
            }
            return [
                KeyBinding("Escape", "stop the quick clear") { store.closeQuickClear() },
                KeyBinding(declining: "e", "done", handler: onItem { sprint.done($0, via: .key) }),
                KeyBinding(declining: "d", "done", handler: onItem { sprint.done($0, via: .key) }),
                KeyBinding(declining: "h", "remind me later", handler: onItem { sprint.later($0) }),
                KeyBinding(declining: "ArrowLeft", "back to the last one passed") {
                    guard case .item = phase else { return false }
                    sprint.unskip()
                    return true
                },
                KeyBinding(declining: "ArrowRight", "not now") { pass() },
                KeyBinding(declining: "Space", "not now") { pass() },
                KeyBinding(declining: "Enter", "open") {
                    switch phase {
                    case .smart:
                        Task { await sprint.runSmartClear() }
                    case .item(let item):
                        sprint.open(item)
                    case .summary:
                        if sprint.left > 0 { sprint.keepGoing() } else { store.closeQuickClear() }
                    }
                    return true
                },
                KeyBinding(declining: "u", "undo") {
                    if case .summary = phase { return false }
                    Task { await sprint.undo() }
                    return true
                },
            ]
        }

        private func pass() -> Bool {
            switch phase {
            case .smart:
                sprint.passSmartClear()
            case .item(let item):
                sprint.skip(item)
            case .summary:
                return false
            }
            return true
        }
    }

    // MARK: - pieces

    /// The item's well: an even hairline all the way round, never one edge.
    private struct Well: View {
        var body: some View {
            RoundedRectangle(cornerRadius: Radius.well, style: .continuous)
                .fill(Palette.hairline.opacity(0.32))
                .overlay(
                    RoundedRectangle(cornerRadius: Radius.well, style: .continuous)
                        .strokeBorder(Palette.hairline, lineWidth: 0.5))
        }
    }

    /// The item on screen: who, then what it is in a reading face. A click
    /// opens it, like a row on the board.
    private struct ItemCard: View {
        let item: QuickClearSignal.Item
        let open: () -> Void

        var body: some View {
            Button(action: open) {
                VStack(alignment: .leading, spacing: Space.s12) {
                    HStack(spacing: Space.s10) {
                        Avatar(sender: item.sender, size: 26)
                        Text(SenderCache.resolved(item.sender).displayName)
                            .font(Typo.ui(13, weight: .medium))
                            .foregroundStyle(Palette.ink)
                            .lineLimit(1)
                        Spacer(minLength: Space.s8)
                        if let kind = QuickClearQueue.kindLabel(item.kind) {
                            Chip(text: kind)
                        }
                        if let est = QuickClearQueue.estimateLabel(item.est_secs) {
                            Text(est)
                                .font(Typo.num(11))
                                .foregroundStyle(Palette.inkFaintest)
                        }
                    }
                    Text(item.one_line.isEmpty ? "No summary yet" : item.one_line)
                        .font(Typo.serif(20))
                        .foregroundStyle(item.one_line.isEmpty ? Palette.inkFaint : Palette.ink)
                        .lineLimit(3)
                        .multilineTextAlignment(.leading)
                        .fixedSize(horizontal: false, vertical: true)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
                .padding(Space.s20)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .background(Well())
        }
    }

    /// One of the two waiting underneath.
    private struct NextRow: View {
        let item: QuickClearSignal.Item

        var body: some View {
            HStack(spacing: Space.s8) {
                Avatar(sender: item.sender, size: 16)
                Text(SenderCache.resolved(item.sender).displayName)
                    .font(Typo.ui(12, weight: .medium))
                    .foregroundStyle(Palette.inkDim)
                    .lineLimit(1)
                    .layoutPriority(1)
                Text(item.one_line)
                    .font(Typo.rowSub)
                    .foregroundStyle(Palette.inkFaint)
                    .lineLimit(1)
                    .truncationMode(.tail)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
    }

    /// The keys, taught in place the way the rest of the app teaches them.
    private struct KeyHints: View {
        let hints: [(String, String)]

        var body: some View {
            HStack(spacing: Space.s14) {
                ForEach(hints, id: \.0) { key, label in
                    HStack(spacing: Space.s4) {
                        Kbd(key)
                        Text(label)
                            .font(Typo.micro)
                            .foregroundStyle(Palette.inkFaintest)
                    }
                }
                Spacer(minLength: 0)
            }
        }
    }

    /// The budget, gently: elapsed time on a thin accent track that fills
    /// toward the budget and never changes colour. Past it, one quiet line
    /// instead of a number that keeps climbing.
    private struct ClockTrack: View {
        let startedAt: Date
        let budget: Int

        var body: some View {
            TimelineView(.periodic(from: startedAt, by: 1)) { ctx in
                let elapsed = max(0, Int(ctx.date.timeIntervalSince(startedAt)))
                let fraction = min(1, Double(elapsed) / Double(max(1, budget)))
                HStack(spacing: Space.s10) {
                    GeometryReader { geo in
                        ZStack(alignment: .leading) {
                            Capsule().fill(Palette.hairline)
                            Capsule()
                                .fill(Palette.accent.opacity(0.55))
                                .frame(width: max(3, geo.size.width * fraction))
                                .animation(.linear(duration: 1), value: elapsed)
                        }
                    }
                    .frame(height: 3)
                    Text(
                        elapsed < budget
                            ? "\(QuickClearQueue.clock(elapsed)) of \(QuickClearQueue.clock(budget))"
                            : QuickClearQueue.timeUpLine(budget)
                    )
                    .font(Typo.num(11))
                    .foregroundStyle(Palette.inkFaint)
                    .fixedSize()
                }
            }
            .accessibilityElement(children: .combine)
        }
    }
#endif
