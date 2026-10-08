// The composer's drafts page: the second page of the compose pane, behind the
// drafts control in its header. Same strip, same header, the body swaps: the
// fields, editor and footer step aside and the list takes the whole pane.
//
// A PAGE, NOT A POPOVER. The popover it replaced was a bubble a third of the
// pane wide hanging off a small word, with its own focus fight (the body's
// text view kept the keyboard behind it) and its own height hack (a scroller
// in a popover has none). As the pane's content none of that exists: the
// text views are unmounted, so there is nothing to steal keys from, and the
// list simply fills the pane.
//
// THE HEADER IS THE WAY BACK. Its control reads "back to new message" (or
// "back to your reply") while this page is up, and Esc does the same, so the
// list itself is only drafts: every one the daemon holds, newest edit first,
// minus mail a held send owns (`AppStore.listedDrafts`). An earlier cut put a
// "start a new message" row on top as well; the user dropped it (2026-09-29)
// as saying the header's thing twice. The one this pane is editing stays
// listed, marked "editing" and inert, so the count in the header and the
// rows here never disagree about what exists.
//
// KEYS (Mac), registered in the pane's own `.modal` context: arrows move the
// cursor over inert rows, Enter opens, Backspace/Delete deletes behind the
// store's undo toast, Esc goes back to the composer when it holds something
// and closes the pane when it does not (a blank ⌘N that landed here has
// nothing to go back to). None of them opt into input: on this page there is
// no text field to share keys with. The pane's own Escape declines while
// this page is up, so which set registered first does not matter.

import SwiftUI

struct DraftsList: View {
    @Environment(AppStore.self) private var store

    /// An index into `rows`; meaningless while the list is empty.
    @State private var cursor = 0
    @State private var loaded = false

    /// Newest first, by the edit rather than the creation: the draft you
    /// touched last is the one you are most likely back for.
    private var rows: [DraftView] {
        store.listedDrafts.sorted {
            (Fmt.date($0.updated_at) ?? .distantPast) > (Fmt.date($1.updated_at) ?? .distantPast)
        }
    }

    /// Whether the composer behind this page holds anything to go back to:
    /// words, a saved row, or a parent (a reply or forward is somebody's
    /// mail even untouched). Decides what Esc does and how the hint names it.
    /// The row id through `DraftSaver.currentDraftId`, as `isInert` reads it:
    /// a first save that came back after the slot last changed counts too.
    private var returning: Bool {
        guard let compose = store.compose else { return false }
        return DraftSaver.hasContent(compose)
            || DraftSaver.shared.currentDraftId(of: compose) != nil
            || compose.replyToMessageId != nil || compose.forwardOfMessageId != nil
    }

    var body: some View {
        let rows = rows
        VStack(alignment: .leading, spacing: 0) {
            ScrollViewReader { proxy in
                ScrollView {
                    VStack(alignment: .leading, spacing: 1) {
                        if rows.isEmpty {
                            note(loaded ? "no other drafts" : "loading…")
                        } else {
                            ForEach(Array(rows.enumerated()), id: \.element.id) { index, draft in
                                DraftRow(
                                    draft: draft,
                                    inert: store.isInert(draft),
                                    selected: showsCursor && cursor == index,
                                    onHover: { cursor = index },
                                    onOpen: { open(draft) },
                                    onDelete: { delete(draft) }
                                )
                                .id(draft.id)
                            }
                            // "No other drafts" also when the only row is the
                            // one being edited: listed, but not another draft.
                            if rows.allSatisfy({ store.isInert($0) }) {
                                note("no other drafts").padding(.top, Space.s4)
                            }
                        }
                    }
                    // 7 + the row's own 11 puts every row's text on the
                    // header's 18pt edge. Written as the difference so the
                    // two still meet in comfortable, where neither is 7 or 11.
                    .padding(.horizontal, Space.scaled(18) - Space.scaled(11))
                    .padding(.vertical, Space.s10)
                    .frame(maxWidth: .infinity, alignment: .leading)
                }
                .onChange(of: cursor) { _, next in
                    if rows.indices.contains(next) { proxy.scrollTo(rows[next].id) }
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)

            #if os(macOS)
                hints
            #endif
        }
        .keyBindings(.modal, bindings(rows))
        .task {
            await store.refreshDrafts()
            loaded = true
        }
        // Rows arrived, left or REORDERED under the cursor (a save bumps a
        // row's `updated_at` to the top at the same count), and on APPEAR:
        // a page that opens with the row being edited at the top would
        // otherwise leave the cursor on it, inert, nothing highlighted, and
        // Enter and ⌫ dead.
        .onChange(of: rows.map(\.id), initial: true) { _, _ in
            normaliseCursor(in: rows)
        }
    }

    /// Keep the cursor on a row that can be opened, preferring the one just
    /// above where it was.
    private func normaliseCursor(in rows: [DraftView]) {
        if cursor >= rows.count { cursor = max(rows.count - 1, 0) }
        if rows.indices.contains(cursor), store.isInert(rows[cursor]) {
            cursor = step(from: cursor, by: -1, in: rows)
                ?? step(from: cursor, by: 1, in: rows) ?? cursor
        }
    }

    /// The selection fill is the KEYBOARD's cursor. A phone has none, so a
    /// fill parked on the first row there would be a permanent highlight
    /// that means nothing (a tap opens whichever row it lands on).
    private var showsCursor: Bool {
        #if os(macOS)
            true
        #else
            false
        #endif
    }

    private func note(_ text: String) -> some View {
        Text(text)
            .font(Typo.micro)
            .foregroundStyle(Palette.inkFaintest)
            .padding(.horizontal, Space.scaled(11))
            .padding(.vertical, Space.s6)
    }

    #if os(macOS)
        /// One muted line where the composer's footer was: this page's keys.
        /// Delete only while a draft is under the cursor, because a hint for a
        /// key that does nothing here is worse than none.
        private var hints: some View {
            HStack(spacing: Space.s4) {
                // Move, open and delete only while there is a draft under
                // the cursor: none on an empty list, none when the only row
                // is the one being edited.
                if rows.indices.contains(cursor), !store.isInert(rows[cursor]) {
                    KeyHint(["↑", "↓"], "move")
                    dot
                    KeyHint("↵", "open")
                    dot
                    KeyHint("⌫", "delete")
                    dot
                }
                KeyHint("esc", returning ? "back" : "close")
                Spacer()
            }
            .padding(.horizontal, Space.scaled(18))
            .padding(.vertical, Space.s12)
            .overlay(alignment: .top) { Hairline() }
        }

        private var dot: some View {
            Text("·").font(Typo.micro).foregroundStyle(Palette.inkFaintest)
        }
    #endif

    /// Back to the composer, as it is: blank, or whatever was written.
    private func back() {
        store.compose?.page = .compose
    }

    private func open(_ draft: DraftView) {
        guard !store.isInert(draft) else { return }
        store.openDraft(draft)
    }

    private func delete(_ draft: DraftView) {
        guard !store.isInert(draft) else { return }
        store.deleteDraft(draft)
    }

    /// The next cursor position, stepping over inert rows: the one being
    /// edited cannot be opened or deleted, so landing on it would be a
    /// keystroke that does nothing. nil past either end.
    private func step(from index: Int, by delta: Int, in rows: [DraftView]) -> Int? {
        var i = index + delta
        while rows.indices.contains(i) {
            if !store.isInert(rows[i]) { return i }
            i += delta
        }
        return nil
    }

    private func bindings(_ rows: [DraftView]) -> [KeyBinding] {
        let current = rows.indices.contains(cursor) ? rows[cursor] : nil
        return [
            KeyBinding("Escape", returning ? "back to the message" : "close") {
                if returning { back() } else { store.closeCompose() }
            },
            KeyBinding("ArrowDown", "next draft") {
                if let next = step(from: cursor, by: 1, in: rows) { cursor = next }
            },
            KeyBinding("ArrowUp", "previous draft") {
                if let next = step(from: cursor, by: -1, in: rows) { cursor = next }
            },
            KeyBinding("Enter", "open draft") {
                if let current { open(current) }
            },
            // DECLINED on an empty list or an inert row: nothing to delete,
            // so the key goes on rather than vanishing into a no-op.
            KeyBinding(declining: "Backspace", "delete draft") {
                guard let current, !store.isInert(current) else { return false }
                delete(current)
                return true
            },
            KeyBinding(declining: "Delete", "delete draft") {
                guard let current, !store.isInert(current) else { return false }
                delete(current)
                return true
            },
        ]
    }
}

/// One draft, in SentRow's envelope and metrics, laid out on two lines
/// because the pane is half the sent page's width, and a phone less.
///
/// NO LEFT ACCENT BAR, which SentRow still draws: a one-sided bar on a
/// rounded selection reads as decoration, and the selection fill already
/// says which row the keys act on.
///
/// THE SLOT SAYS THE RELATION. SentRow puts "to" in a fixed-width slot before
/// the recipient; here the same slot carries `to` / `re` / `fwd`, and what
/// follows it is the thing that relation points at: the people for new mail,
/// the parent's subject for a reply or forward (with its sender, muted). So
/// the kind of draft is read in the same glance as the who, and never said
/// twice.
private struct DraftRow: View {
    let draft: DraftView
    let inert: Bool
    let selected: Bool
    let onHover: () -> Void
    let onOpen: () -> Void
    let onDelete: () -> Void

    var body: some View {
        ListRow(
            selected: selected && !inert, cornerRadius: 8, hPadding: Space.scaled(11), vPadding: Space.s6,
            hoverFill: !inert,
            onHoverChange: inert ? nil : { if $0 { onHover() } },
            action: {
                guard !inert else { return }
                onHover()
                onOpen()
            }
        ) { _, _ in
            // The slot, the who, the subject and snippet and the file count
            // are `CompositionSummary`, shared with the outbox's rows on
            // Sent; the editing mark and the age are this row's own.
            CompositionSummary(draft: draft) {
                if inert {
                    Text("editing")
                        .font(Typo.micro)
                        .foregroundStyle(Palette.accent)
                } else if draft.from_agent == true {
                    // Whose words these are, before they are sent as yours.
                    Text("from your agent")
                        .font(Typo.micro)
                        .foregroundStyle(Palette.accent)
                        .help("An agent connected over Agent MCP wrote this draft.")
                }
                Text(Fmt.relAge(draft.updated_at))
                    .font(Typo.num(10))
                    .foregroundStyle(Palette.inkFaintest)
                    .frame(minWidth: Space.scaled(26), alignment: .trailing)
            }
        }
        .allowsHitTesting(!inert)
        .opacity(inert ? 0.62 : 1)
        // The phone's way to delete (a long press), and a second way on the
        // Mac: there is no Backspace on a touch screen.
        .contextMenu {
            if !inert {
                Button("open") { onOpen() }
                Button("delete", role: .destructive) { onDelete() }
            }
        }
    }
}
