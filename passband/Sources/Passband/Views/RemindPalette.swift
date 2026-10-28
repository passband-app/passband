// REMIND PALETTE — `h` on a focused email: say when, hit Enter, and the mail
// leaves now and comes back then.
//
// EVERY ROW STATES ITS ABSOLUTE TIME. This is the one palette in the app whose
// pick is a promise about the future, and "next week" is a different Monday
// depending on which day you asked — so the words you typed sit on the left and
// the instant they resolve to sits on the right, on every row, always. The
// suggestion engine underneath (RemindTimes) is pure and asserted; this file is
// only the surface.

import SwiftUI

struct RemindPalette: View {
    let target: RemindTarget
    let onClose: () -> Void

    @State private var query = ""
    @State private var selection = 0
    @State private var busy = false
    /// Pinned when the palette opens: every row is scored against ONE clock, so
    /// a list cannot re-rank itself under the cursor between keystrokes.
    @State private var opened = Date()
    @FocusState private var focused: Bool

    /// The rows on screen. STATE, not a computed property: SwiftUI reads a
    /// computed one several times per render, and every read re-ran the whole
    /// engine. One recompute per thing that can actually change the answer —
    /// typing, opening, re-pinning the clock — is the palette's entire typing
    /// latency, so that is all it gets.
    @State private var hits: [RemindHit] = []

    var body: some View {
        surface
            .keyContext(.modal)
            .keyBindings(.modal, bindings)
            .onAppear {
                // A tick late: on the frame the palette appears the field is
                // not in the window yet, and the reader's web view keeps the
                // keyboard, so a same-frame focus is dropped.
                DispatchQueue.main.async { focused = true }
                refreshHits()
            }
    }

    /// Re-run the engine against the pinned clock, keeping the cursor inside
    /// the new list.
    private func refreshHits() {
        hits = RemindTimes.match(query, now: opened)
        selection = max(0, min(selection, max(0, hits.count - 1)))
    }

    private var surface: some View {
        PaletteShell(
            glassID: "remind", icon: "bell.badge",
            title: rescheduling ? "Move the reminder" : "Remind me",
            subject: target.subject, subjectHelp: "\(target.sender) · \(target.subject)",
            // WHAT IT IS SET TO NOW, when there is one. A second `h` is a move,
            // not a first booking, and the row you are moving off is the one
            // fact the list itself cannot show.
            current: target.remindAt.flatMap { $0.isEmpty ? nil : "now \(Fmt.remindChip($0))" },
            isEmpty: hits.isEmpty,
            emptyText:
                "no time in “\(query)”. try a day (\"friday\", \"aug 30\"), a wait (\"in 3 hours\") or leave it empty for the usual ones.",
            selection: selection, idAt: { hits[safe: $0]?.id },
            verb: "set",
            // The half of this that is not obvious: the mail goes away NOW.
            note: "marks it done until then",
            onClose: onClose
        ) {
            TextField("remind when...", text: $query)
                .focused($focused)
                .disabled(busy)
                #if !os(macOS)
                    // Return sets the highlighted row, which is what Enter does
                    // through the keymap on the Mac. Without it the highlight on a
                    // phone marks a row for a key that never arrives.
                    .submitLabel(.done)
                    .onSubmit {
                        if let hit = hits[safe: selection] { Task { await apply(hit) } }
                    }
                #endif
                .onChange(of: query) { _, _ in
                    selection = 0
                    refreshHits()
                }
        } rows: {
            ForEach(Array(hits.enumerated()), id: \.element.id) { i, hit in
                RemindRow(
                    hit: hit, selected: i == selection,
                    onHover: { selection = i },
                    onPick: { Task { await apply(hit) } })
                .id(hit.id)
                .disabled(busy)
            }
        }
    }

    private var rescheduling: Bool { !(target.remindAt ?? "").isEmpty }

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
            KeyBinding("Enter", "set reminder", allowInInput: true) {
                if let hit = hits[safe: selection] { Task { await apply(hit) } }
            },
        ]
    }

    /// Set it. Then, and only on a stamp that actually landed, close and hand
    /// back to whoever opened us.
    ///
    /// THE PALETTE STAYS OPEN ON FAILURE, with what you typed still in it. The
    /// close and the caller's `onScheduled` (the reader walks to the next email
    /// on it) are the app saying the reminder took, and running that
    /// choreography over a 400 leaves the user watching the mail leave while a
    /// toast says it did not. So both hang off `Actions.remind`'s answer.
    private func apply(_ hit: RemindHit) async {
        guard !busy else { return }
        busy = true
        // THE PINNED CLOCK'S ESCAPE HATCH. `opened` is deliberately frozen so
        // the list cannot re-rank itself under the cursor between keystrokes —
        // which also means a palette left open long enough holds rows that have
        // since gone by, and the daemon 400s a reminder set in the past. Rather
        // than submit that, re-pin the clock and let the list re-resolve: the
        // same words now mean a time that is still ahead.
        guard hit.date > Date() else {
            opened = Date()
            selection = 0
            busy = false
            // `hits` is state now, so moving the clock does nothing until the
            // engine is re-asked.
            refreshHits()
            return
        }
        let ok = await Actions.remind(target.messageId, at: hit.date, label: hit.detail)
        guard ok else {
            busy = false
            return
        }
        onClose()
        target.onScheduled?()
    }
}

/// One pick: the words on the left, the instant they resolve to on the right.
/// Shared with `SchedulePalette`, which asks the same engine a different
/// question ("send when") and must answer it in the same shape.
struct RemindRow: View {
    let hit: RemindHit
    let selected: Bool
    let onHover: () -> Void
    let onPick: () -> Void

    var body: some View {
        PaletteRow(
            selected: selected, tint: Palette.accent, onHover: onHover, onPick: onPick
        ) {
            HStack(spacing: Space.s10) {
                Text(hit.label)
                    .font(Typo.ui(13, weight: .medium))
                    .foregroundStyle(Palette.ink)
                    .lineLimit(1)
                    .frame(maxWidth: .infinity, alignment: .leading)
                // THE ANSWER, on every row without exception.
                Text(hit.detail)
                    .font(Typo.micro)
                    .foregroundStyle(Palette.accent.opacity(0.85))
                    .fixedSize()
            }
        }
    }
}
