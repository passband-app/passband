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
// THE FIRST ROW IS THE WAY BACK. "start a new message" (or "back to your
// message", when the composer holds something) sits on top, selected by
// default, so a ⌘N that landed here (see `AppStore.openComposeNew`) is one
// Enter from a blank composer. Below it every draft the daemon holds, newest
// edit first, minus mail a held send owns (`AppStore.listedDrafts`). The one
// this pane is editing stays listed, marked "editing" and inert, so the count
// in the header and the rows here never disagree about what exists.
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

    /// 0 is the new-message row; `i` above it is `rows[i - 1]`.
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
    /// mail even untouched). Decides the first row's words and what Esc does.
    private var returning: Bool {
        guard let compose = store.compose else { return false }
        return DraftSaver.hasContent(compose) || compose.draftId != nil
            || compose.replyToMessageId != nil || compose.forwardOfMessageId != nil
    }

    private var kindWord: String {
        if store.compose?.forwardOfMessageId != nil { return "forward" }
        return store.compose?.replyToMessageId != nil ? "reply" : "message"
    }

    private static let newRowID = "drafts-list-new-message"

    var body: some View {
        let rows = rows
        VStack(alignment: .leading, spacing: 0) {
            ScrollViewReader { proxy in
                ScrollView {
                    VStack(alignment: .leading, spacing: 1) {
                        NewMessageRow(
                            label: returning ? "back to your \(kindWord)" : "start a new message",
                            returning: returning,
                            selected: cursor == 0,
                            onHover: { cursor = 0 },
                            onOpen: back
                        )
                        .id(Self.newRowID)

                        Text("drafts")
                            .font(Typo.sectionLabel)
                            .foregroundStyle(Palette.inkFaint)
                            .textCase(.uppercase)
                            .padding(.horizontal, 11)
                            .padding(.top, 16)
                            .padding(.bottom, 5)

                        if rows.isEmpty {
                            note(loaded ? "no other drafts" : "loading…")
                        } else {
                            ForEach(Array(rows.enumerated()), id: \.element.id) { index, draft in
                                DraftRow(
                                    draft: draft,
                                    inert: store.isInert(draft),
                                    selected: cursor == index + 1,
                                    onHover: { cursor = index + 1 },
                                    onOpen: { open(draft) },
                                    onDelete: { delete(draft) }
                                )
                                .id(draft.id)
                            }
                            // "No other drafts" also when the only row is the
                            // one being edited: listed, but not another draft.
                            if rows.allSatisfy({ store.isInert($0) }) {
                                note("no other drafts").padding(.top, 4)
                            }
                        }
                    }
                    // 7 + the row's own 11 puts every row's text on the
                    // header's 18pt edge.
                    .padding(.horizontal, 7)
                    .padding(.vertical, 10)
                    .frame(maxWidth: .infinity, alignment: .leading)
                }
                .onChange(of: cursor) { _, next in
                    if next == 0 {
                        proxy.scrollTo(Self.newRowID)
                    } else if rows.indices.contains(next - 1) {
                        proxy.scrollTo(rows[next - 1].id)
                    }
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
        .onChange(of: rows.count) { _, count in
            if cursor > count { cursor = count }
            if cursor > 0, store.isInert(rows[cursor - 1]) {
                cursor = step(from: cursor, by: -1, in: rows) ?? 0
            }
        }
    }

    private func note(_ text: String) -> some View {
        Text(text)
            .font(Typo.micro)
            .foregroundStyle(Palette.inkFaintest)
            .padding(.horizontal, 11)
            .padding(.vertical, 6)
    }

    #if os(macOS)
        /// One muted line where the composer's footer was: this page's keys.
        /// Delete only while a draft is under the cursor, because a hint for a
        /// key that does nothing here is worse than none.
        private var hints: some View {
            HStack(spacing: 4) {
                KeyHint(["↑", "↓"], "move")
                dot
                KeyHint("↵", "open")
                if cursor > 0 {
                    dot
                    KeyHint("⌫", "delete")
                }
                dot
                KeyHint("esc", returning ? "back" : "close")
                Spacer()
            }
            .padding(.horizontal, 18)
            .padding(.vertical, 12)
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
    /// keystroke that does nothing. The new-message row (0) is always a stop.
    /// nil past either end.
    private func step(from index: Int, by delta: Int, in rows: [DraftView]) -> Int? {
        var i = index + delta
        while i >= 0 && i <= rows.count {
            if i == 0 || !store.isInert(rows[i - 1]) { return i }
            i += delta
        }
        return nil
    }

    private func bindings(_ rows: [DraftView]) -> [KeyBinding] {
        let current = cursor > 0 && rows.indices.contains(cursor - 1) ? rows[cursor - 1] : nil
        return [
            KeyBinding("Escape", "back to the message") {
                if returning { back() } else { store.closeCompose() }
            },
            KeyBinding("ArrowDown", "next draft") {
                if let next = step(from: cursor, by: 1, in: rows) { cursor = next }
            },
            KeyBinding("ArrowUp", "previous draft") {
                if let next = step(from: cursor, by: -1, in: rows) { cursor = next }
            },
            KeyBinding("Enter", "open draft") {
                if cursor == 0 {
                    back()
                } else if let current {
                    open(current)
                }
            },
            // DECLINED on the new-message row or an inert one: nothing to
            // delete, so the key goes on rather than vanishing into a no-op.
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

/// The list's first row: back to the composer. One line in the drafts rows'
/// metrics, its glyph in their relation slot, so it reads as the head of the
/// same column rather than a button parked above it.
private struct NewMessageRow: View {
    let label: String
    let returning: Bool
    let selected: Bool
    let onHover: () -> Void
    let onOpen: () -> Void

    var body: some View {
        ListRow(
            selected: selected, cornerRadius: 8, hPadding: 11, vPadding: 8,
            onHoverChange: { if $0 { onHover() } },
            action: {
                onHover()
                onOpen()
            }
        ) { _, _ in
            HStack(alignment: .center, spacing: 9) {
                Image(systemName: returning ? "arrow.uturn.backward" : "square.and.pencil")
                    .font(.system(size: 11, weight: .semibold))
                    .foregroundStyle(Palette.accent)
                    .frame(width: 26, alignment: .trailing)
                Text(label)
                    .font(.system(size: 12, weight: .medium))
                    .foregroundStyle(Palette.ink)
                    .lineLimit(1)
                    .frame(maxWidth: .infinity, alignment: .leading)
                #if os(macOS)
                    Kbd("⌘N")
                #endif
            }
        }
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

    private enum Kind { case new, reply, forward }

    private var kind: Kind {
        if draft.forward_of_message_id != nil { return .forward }
        return draft.reply_to_message_id != nil ? .reply : .new
    }

    private var slot: String {
        switch kind {
        case .new: "to"
        case .reply: "re"
        case .forward: "fwd"
        }
    }

    private var names: String {
        SenderID.recipients(draft.to)
            .map { SenderCache.resolved($0).displayName }
            .joined(separator: ", ")
    }

    /// The first line with words on it, skipping quoted mail, so a reply's
    /// snippet is what you wrote rather than what you were answering.
    private var snippet: String {
        draft.body
            .split(whereSeparator: \.isNewline)
            .lazy
            .map { $0.trimmingCharacters(in: .whitespaces) }
            .first { !$0.isEmpty && !$0.hasPrefix(">") } ?? ""
    }

    private var fileCount: Int { draft.attachments?.count ?? 0 }

    var body: some View {
        ListRow(
            selected: selected && !inert, cornerRadius: 8, hPadding: 11, vPadding: 6,
            hoverFill: !inert,
            onHoverChange: inert ? nil : { if $0 { onHover() } },
            action: {
                guard !inert else { return }
                onHover()
                onOpen()
            }
        ) { _, _ in
            HStack(alignment: .firstTextBaseline, spacing: 9) {
                Text(slot)
                    .font(Typo.micro)
                    .foregroundStyle(Palette.inkFaintest)
                    .textCase(.uppercase)
                    .frame(width: 26, alignment: .trailing)

                VStack(alignment: .leading, spacing: 2) {
                    HStack(alignment: .firstTextBaseline, spacing: 6) {
                        who
                            .frame(maxWidth: .infinity, alignment: .leading)
                        trailing
                    }
                    detail
                }
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

    /// Line one: who it is to, or what it answers.
    @ViewBuilder
    private var who: some View {
        switch kind {
        case .new:
            Text(names.isEmpty ? "no recipient" : names)
                .font(.system(size: 12, weight: .medium))
                .foregroundStyle(names.isEmpty ? Palette.inkFaintest : Palette.ink)
                .lineLimit(1)
                .truncationMode(.middle)
        case .reply, .forward:
            HStack(alignment: .firstTextBaseline, spacing: 5) {
                Text(parentSubject)
                    .font(.system(size: 12, weight: .medium))
                    .foregroundStyle(Palette.ink)
                    .lineLimit(1)
                    .truncationMode(.tail)
                    .layoutPriority(1)
                if let from = draft.parent_from?.trimmed, !from.isEmpty {
                    Text(from)
                        .font(Typo.rowSub)
                        .foregroundStyle(Palette.inkFaintest)
                        .lineLimit(1)
                        .truncationMode(.tail)
                }
            }
        }
    }

    private var parentSubject: String {
        let s = draft.parent_subject?.trimmed ?? ""
        return s.isEmpty ? "(no subject)" : s
    }

    /// Line two: the subject and the first line of the body. A reply or
    /// forward with no subject of its own shows none: the daemon titles it
    /// from the parent, which line one already names.
    @ViewBuilder
    private var detail: some View {
        let subject = draft.subject.trimmed
        let showSubject = !subject.isEmpty || kind == .new
        if showSubject || !snippet.isEmpty {
            HStack(spacing: 6) {
                if showSubject {
                    Text(subject.isEmpty ? "(no subject)" : subject)
                        .font(Typo.rowSub)
                        .foregroundStyle(subject.isEmpty ? Palette.inkFaintest : Palette.inkDim)
                        .lineLimit(1)
                        .truncationMode(.tail)
                        .layoutPriority(1)
                }
                if !snippet.isEmpty {
                    Text(snippet)
                        .font(Typo.rowSub)
                        .foregroundStyle(Palette.inkFaintest)
                        .lineLimit(1)
                        .truncationMode(.tail)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    /// Files, the editing mark, and the age, right-aligned on line one.
    private var trailing: some View {
        HStack(spacing: 7) {
            if fileCount > 0 {
                HStack(spacing: 2) {
                    Image(systemName: "paperclip")
                        .font(.system(size: 9, weight: .semibold))
                    Text("\(fileCount)")
                        .font(Typo.num(10))
                }
                .foregroundStyle(Palette.inkFaint)
                .accessibilityLabel(fileCount == 1 ? "1 file" : "\(fileCount) files")
            }
            if inert {
                Text("editing")
                    .font(Typo.micro)
                    .foregroundStyle(Palette.accent)
            }
            Text(Fmt.relAge(draft.updated_at))
                .font(Typo.num(10))
                .foregroundStyle(Palette.inkFaintest)
                .frame(minWidth: 26, alignment: .trailing)
        }
        .fixedSize()
        .layoutPriority(3)
    }
}
