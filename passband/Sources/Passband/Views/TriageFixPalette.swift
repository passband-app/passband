// Explicit corrections for independent placement, kind, and agent access.
// The owner always keeps human access, including after an access restriction.

import SwiftUI

struct TriageFixPalette: View {
    let target: TriageFixTarget
    let onClose: () -> Void

    @Environment(AppStore.self) private var store
    @State private var query = ""
    @State private var selection = 0
    @State private var busy = false
    @State private var current: AgentMessageDecision?
    @FocusState private var focused: Bool

    private var hits: [TriageTarget] { TriageTargets.match(query) }

    var body: some View {
        surface
            .keyContext(.modal)
            .keyBindings(.modal, bindings)
            .onAppear {
                // A tick late: on the frame the palette appears the field is
                // not in the window yet, and the reader's web view keeps the
                // keyboard, so a same-frame focus is dropped.
                DispatchQueue.main.async { focused = true }
            }
            .task { current = try? await APIClient.shared.getAgentTriage(target.messageId).decision }
            .onChange(of: hits.count) { _, count in
                selection = max(0, min(selection, max(0, count - 1)))
            }
    }

    /// The palette itself. Same rows, same input, same apply: only the thing it
    /// is mounted IN differs, a command bar on the Mac or the phone's sheet,
    /// which brings its own scrim and its own dismissal (see `PaletteShell`).
    private var surface: some View {
        PaletteShell(
            glassID: "palette", icon: "wand.and.sparkles", title: "Fix triage",
            subject: target.subject, subjectHelp: "\(target.sender) · \(target.subject)",
            // Display only the current authoritative decision, never legacy tiers.
            detail: current.map {
                ($0.kinds + $0.destinations).joined(separator: " · ")
                    .replacingOccurrences(of: "_", with: " ")
            },
            isEmpty: hits.isEmpty,
            emptyText:
                "nothing matches “\(query)”. these are the only values the triage pipeline itself uses, so anything else could not be learned from.",
            selection: selection, idAt: { hits[safe: $0]?.id },
            listMaxHeight: 300,
            // Higher than the short palettes, as it always opened: its detail
            // line and longer list need the room below.
            topInset: 110,
            verb: "apply",
            note: "stored to refine triage",
            onClose: onClose
        ) {
            TextField("Change placement, kind, or agent access…", text: $query)
                .focused($focused)
                .disabled(busy)
                .onChange(of: query) { _, _ in selection = 0 }
        } rows: {
            ForEach(Array(hits.enumerated()), id: \.element.id) { i, hit in
                TargetRow(
                    target: hit, selected: i == selection,
                    onHover: { selection = i },
                    onPick: { Task { await apply(hit) } })
                .id(hit.id)
                .disabled(busy)
            }
        }
    }

    /// allowInInput is REQUIRED, not polish: the palette autofocuses its field,
    /// so the `editing && !allowInInput` guard would drop every binding — and
    /// Escape would fall through to whatever binds it underneath, meaning
    /// cancelling would NAVIGATE the app.
    private var bindings: [KeyBinding] {
        [
            KeyBinding("Escape", "cancel", allowInInput: true) { onClose() },
            KeyBinding("ArrowDown", "next", allowInInput: true) {
                selection = min(hits.count - 1, selection + 1)
            },
            KeyBinding("ArrowUp", "prev", allowInInput: true) {
                selection = max(0, selection - 1)
            },
            KeyBinding("Enter", "apply", allowInInput: true) {
                if let hit = hits[safe: selection] { Task { await apply(hit) } }
            },
        ]
    }

    /// Corrections preserve unrelated dimensions; refreshed server projections
    /// decide which visible rows change after the write succeeds.
    private func apply(_ hit: TriageTarget) async {
        guard !busy else { return }
        busy = true
        do {
            try await APIClient.shared.correctTriage(
                messageId: target.messageId, target: hit)
        } catch {
            store.pushToast(errText(error, "could not record the correction"), .error)
            busy = false
            return
        }
        Analytics.capture("triage_corrected", ["axis": hit.axis.rawValue, "to": hit.value])
        Discovery.note(.triageCorrected)
        // A restriction changes external access only. The owner keeps the
        // message and its placements. Refresh authoritative projections instead
        // of inferring a move from its kind or access assessment.
        ThreadPrefetch.shared.wipe()
        store.pushToast("\(hit.label) · recorded", .success)
        onClose()
        // Runs on past the dismissal on purpose: the calling Task belongs to the
        // key binding, not to this view, so closing does not cancel it.
        await SitrepPoller.shared.refreshAfterCorrection()
    }
}

private struct TargetRow: View {
    let target: TriageTarget
    let selected: Bool
    let onHover: () -> Void
    let onPick: () -> Void

    private var axisTone: Color {
        switch target.axis {
        case .showInFye: Palette.warn
        case .kinds, .destinations: Palette.accent
        case .externalAccess: Palette.lock
        }
    }

    var body: some View {
        PaletteRow(selected: selected, tint: axisTone, onHover: onHover, onPick: onPick) {
            HStack(spacing: Space.s10) {
                Chip(text: target.axis.chipLabel, tone: axisTone, filled: true)
                    .frame(width: Typo.measure(68), alignment: .leading)
                Text(target.label)
                    .font(Typo.ui(13, weight: .medium))
                    .foregroundStyle(Palette.ink)
                    .frame(width: Typo.measure(130), alignment: .leading)
                Text(target.hint)
                    .font(Typo.micro)
                    .foregroundStyle(Palette.inkFaint)
                    .lineLimit(1)
                    .frame(maxWidth: .infinity, alignment: .leading)

            }
        }
    }
}
