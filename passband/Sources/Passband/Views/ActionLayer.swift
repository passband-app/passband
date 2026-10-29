// The global action surface, above every other layer: undo/notice toasts, the
// unsubscribe-violation notice (Views/UnsubscribeViolationNotice.swift), rule editor, the 2FA modal,
// triage-fix palette, ⌘K ask bar, shortcuts overlay, the re-triage confirm and
// the re-triage run itself. The composer is NOT here anymore — it is a layout
// pane (see ComposePane in MainShell), not an overlay.
// Read views own the list keymap; this layer extends that same "list" context
// with c. Modal-context keys live inside each overlay.

import SwiftUI

struct ActionLayer: View {
    @Environment(AppStore.self) private var store

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
            .padding(.bottom, ToastMetrics.edge)

            // AUTOMATIC UNSUBSCRIBES, bottom-right of the sitrep: the toasts'
            // opposite corner, so a job and an undo chip never fight for one.
            // Sitrep only, with no reader or composer over it: `u` leaves the
            // reader on purpose, and this is where the user lands.
            //
            // THE SMART CLEAR RECEIPT SHARES THE CORNER, under the tray: one
            // column pinned to the bottom edge, so a job starting while a
            // receipt is up pushes nothing sideways and covers nothing.
            if showsUnsubscribeTray || showsSmartClearReceipt {
                VStack(spacing: 0) {
                    Spacer(minLength: 0)
                    HStack(spacing: 0) {
                        Spacer(minLength: 0)
                        VStack(alignment: .trailing, spacing: Space.s10) {
                            if showsUnsubscribeTray {
                                UnsubscribeTray().fixedSize()
                                    .transition(.opacity)
                            }
                            if showsSmartClearReceipt, let receipt = store.smartClearReceipt {
                                SmartClearReceiptCard(receipt: receipt)
                                    .id(receipt.id)
                                    .transition(
                                        .move(edge: .trailing).combined(with: .opacity))
                            }
                        }
                    }
                }
                .padding(.trailing, ToastMetrics.edge)
                .padding(.bottom, ToastMetrics.edge)
                .transition(.opacity)
                .animation(.smooth(duration: 0.3), value: store.smartClearReceipt?.id)
                .animation(.smooth(duration: 0.26), value: showsUnsubscribeTray)
            }

            // THE DISCOVERY RING'S FOUND MOMENT. Above the toasts and the
            // tray, under every palette and modal below: the moment only ever
            // arrives when none of them is up, and one raised over it covers it.
            DiscoveryLayer()

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
            // Auth mail opened from outside the Auth page: an auth row in For
            // your eyes, or a notification tap on auth mail.
            if let meta = store.revealRequest {
                RevealPanel(meta: meta, source: store.revealSource) { store.revealRequest = nil }
                    .id(meta.id)
            }
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
            // The composer's trash can asking first. Here for the reason
            // the clock above is: the pane is too narrow to hold a scrim.
            if store.discardAsk != nil { DiscardConfirm() }
            if store.askBarOpen { AskBar { store.askBarOpen = false } }
            if store.shortcutsOpen {
                ShortcutsOverlay { store.shortcutsOpen = false }
            }
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
            // The calendar connect card: same slot and same reasoning as the
            // reconnect prompt above, which it is built like. Only for the
            // account it was raised for (`calendarCardUp`): a switch that
            // happens under it takes it down, whatever else is cleared.
            if let ask = store.calendarConnectAsk, store.calendarCardUp {
                CalendarConnectCard(ask: ask)
            }
            // The delete question, the calendar page's own card, for the
            // account it was raised under. (The event editor is not here any
            // more: it is a pane in the shell's layout, like the composer.)
            if let ask = store.calendarDeleteAsk, ask.epoch == store.epoch {
                CalendarDeleteConfirm(ask: ask)
            }
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
        .background { ViolationScanKeeper() }
    }

    /// NOT WHILE THE SMART CLEAR LIST IS UP: it is a page in the content
    /// pane, and the corner's cards belong to the board it covers.
    private var showsUnsubscribeTray: Bool {
        store.showsSitrepCorner && !store.smartClearListOpen
            && !UnsubscribeJobs.shared.items.isEmpty
    }

    private var showsSmartClearReceipt: Bool {
        store.showsSitrepCorner && !store.smartClearListOpen && store.smartClearReceipt != nil
    }

    // MARK: - toasts

    private var toastStack: some View {
        GlassEffectContainer(spacing: 10) {
            VStack(alignment: .leading, spacing: Space.scaled(7)) {
                // Sender strings are email-derived: rendered as Text only,
                // never as markup.
                if let prompt = UnsubscribeViolations.shared.prompt {
                    UnsubscribeViolationNotice(record: prompt)
                        .transition(.move(edge: .leading).combined(with: .opacity))
                }
                ForEach(store.toasts) { toast in
                    Button {
                        store.dismissToast(toast.id)
                    } label: {
                        Text(toast.text)
                            .font(Typo.rowSub)
                            .foregroundStyle(tone(toast.tone))
                            .padding(.horizontal, ToastMetrics.hPadding)
                            .padding(.vertical, ToastMetrics.vPadding)
                    }
                    .buttonStyle(.plain)
                    .background(
                        RoundedRectangle(cornerRadius: Radius.toast, style: .continuous)
                            .fill(.clear)
                    )
                    .toastGlass(tint: tone(toast.tone).opacity(0.16), interactive: true)
                    .transition(.move(edge: .leading).combined(with: .opacity))
                }
                ForEach(store.undos) { undo in
                    Button {
                        Task { await store.fireUndo(undo.id) }
                    } label: {
                        HStack(spacing: Space.s14) {
                            HStack(spacing: Space.s8) {
                                if let expiresAt = undo.expiresAt {
                                    UndoCountdown(
                                        startedAt: undo.createdAt, expiresAt: expiresAt
                                    )
                                }
                                Text(undo.label)
                                    .font(Typo.rowSub)
                                    .foregroundStyle(Palette.ink)
                            }
                            HStack(spacing: Space.s4) {
                                Kbd("u")
                                Text("undo")
                                    .font(Typo.micro)
                                    .foregroundStyle(Palette.inkFaintest)
                            }
                        }
                        .padding(.horizontal, ToastMetrics.hPadding)
                        .padding(.vertical, ToastMetrics.vPadding)
                    }
                    .buttonStyle(.plain)
                    .toastGlass(tint: Palette.warn.opacity(0.18), interactive: true)
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
