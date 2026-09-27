// The composer's drafts picker: the popover behind the "drafts" button in the
// composer's header, beside the kind label. The kind label says what this mail
// IS; the button beside it answers "which mail is this", so it sits there and
// not in the footer, whose left cluster is how a mail goes out.
//
// A popover, not a page: small, dense, one scroller. Every draft the daemon
// holds is listed newest first, except mail a held send owns (the store keeps
// those out; see `AppStore.listedDrafts`). The one the open composer is
// editing stays in the list, marked "editing" and inert, so the count on the
// button and the rows under it never disagree about what exists.
//
// KEYS (Mac): arrows move the cursor, Enter opens, Backspace/Delete deletes
// behind the store's undo toast, Esc closes THIS and not the composer. The
// picker pushes `.modal` and registers its bindings on appear, AFTER the
// composer's own `.modal` set, and dispatch walks the newest set first, so
// its Escape wins while it is up and the composer's is back the moment it
// unmounts.
//
// FOCUS: left alone, the composer's text view keeps first responder behind
// a popover, so a letter typed with the picker open lands in the body
// underneath. The picker takes focus itself on appear (`pickerFocused`),
// which makes the popover's window key and moves first responder off the
// body. The bindings still opt into input, as a backstop: should the focus
// ever fail to take, a picker whose arrows went to the body would be a
// picker you cannot drive. That is also why Backspace/Delete decline (see
// `bindings`) on an inert row or an empty list: with the body possibly
// still listening, a keystroke the picker cannot use must reach it.

import SwiftUI

struct DraftsPicker: View {
    @Environment(AppStore.self) private var store
    /// Close the popover. Called before opening a draft too: opening a reply
    /// closes the pane composer this popover hangs off.
    let onClose: () -> Void

    @State private var cursor = 0
    @State private var loaded = false
    /// The rows' real height, measured. A ScrollView has no height of its
    /// own, so under only a `maxHeight` the Mac popover sized itself to the
    /// first frame (the loading note) and never grew when the rows arrived:
    /// one row, half of it clipped. The scroller gets this height outright,
    /// capped, and scrolls only past the cap.
    @State private var listHeight: CGFloat = 0
    /// Set on appear, so the popover takes the keyboard from the composer's
    /// body; see the header.
    @FocusState private var pickerFocused: Bool

    /// Newest first, by the edit rather than the creation: the draft you
    /// touched last is the one you are most likely back for.
    private var rows: [DraftView] {
        store.listedDrafts.sorted {
            (Fmt.date($0.updated_at) ?? .distantPast) > (Fmt.date($1.updated_at) ?? .distantPast)
        }
    }

    var body: some View {
        let rows = rows
        VStack(alignment: .leading, spacing: 2) {
            Text("drafts")
                .font(Typo.sectionLabel)
                .foregroundStyle(Palette.inkFaint)
                .textCase(.uppercase)
                .padding(.horizontal, 11)
                .padding(.top, 9)
                .padding(.bottom, 3)

            // "No other drafts" also when the only row is the one being
            // edited: that row is listed, but it is not another draft.
            if rows.isEmpty && !loaded {
                note("loading…")
            } else if rows.isEmpty {
                note("no other drafts")
            } else {
                ScrollViewReader { proxy in
                    ScrollView {
                        VStack(spacing: 1) {
                            ForEach(Array(rows.enumerated()), id: \.element.id) { index, draft in
                                DraftRow(
                                    draft: draft,
                                    inert: store.isInert(draft),
                                    selected: index == cursor,
                                    onHover: { cursor = index },
                                    onOpen: { open(draft) },
                                    onDelete: { delete(draft) }
                                )
                                .id(draft.id)
                            }
                            if rows.allSatisfy({ store.isInert($0) }) {
                                note("no other drafts").padding(.top, 4)
                            }
                        }
                        .padding(.horizontal, 5)
                        .padding(.bottom, 5)
                        .onGeometryChange(for: CGFloat.self) { $0.size.height } action: {
                            listHeight = $0
                        }
                    }
                    #if os(macOS)
                    // Floor for the first frame, before the measure lands.
                    .frame(height: min(max(listHeight, 44), 340))
                    #endif
                    .onChange(of: cursor) { _, next in
                        guard rows.indices.contains(next) else { return }
                        proxy.scrollTo(rows[next].id)
                    }
                }
            }
        }
        #if os(macOS)
            .frame(width: 420)
            .focusable()
            .focusEffectDisabled()
            .focused($pickerFocused)
            .onAppear { pickerFocused = true }
        #else
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
            .presentationDetents([.medium, .large])
        #endif
        .keyContext(.modal)
        .keyBindings(.modal, bindings(rows))
        .task {
            await store.refreshDrafts()
            loaded = true
            // Start on the first row that can be opened: the newest is often
            // the one this composer is editing.
            cursor = step(from: -1, by: 1, in: self.rows) ?? 0
        }
        .onChange(of: rows.count) { _, count in
            cursor = min(cursor, max(count - 1, 0))
        }
    }

    private func note(_ text: String) -> some View {
        Text(text)
            .font(Typo.micro)
            .foregroundStyle(Palette.inkFaintest)
            .padding(.horizontal, 11)
            .padding(.bottom, 11)
    }

    private func open(_ draft: DraftView) {
        guard !store.isInert(draft) else { return }
        onClose()
        store.openDraft(draft)
    }

    private func delete(_ draft: DraftView) {
        guard !store.isInert(draft) else { return }
        store.deleteDraft(draft)
    }

    /// The next row the cursor may rest on, stepping over inert rows: the
    /// one being edited cannot be opened or deleted, so landing on it would
    /// be a keystroke that does nothing. nil at either end.
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
            KeyBinding("Escape", "close drafts", allowInInput: true) { onClose() },
            KeyBinding("ArrowDown", "next draft", allowInInput: true) {
                if let next = step(from: cursor, by: 1, in: rows) { cursor = next }
            },
            KeyBinding("ArrowUp", "previous draft", allowInInput: true) {
                if let next = step(from: cursor, by: -1, in: rows) { cursor = next }
            },
            // DECLINED, not swallowed, on no row or an inert one (the draft
            // this composer is editing): the key then goes on to whatever
            // else wants it rather than vanishing into a no-op.
            KeyBinding(declining: "Enter", "open draft", allowInInput: true) {
                guard let current, !store.isInert(current) else { return false }
                open(current)
                return true
            },
            KeyBinding(declining: "Backspace", "delete draft", allowInInput: true) {
                guard let current, !store.isInert(current) else { return false }
                delete(current)
                return true
            },
            KeyBinding(declining: "Delete", "delete draft", allowInInput: true) {
                guard let current, !store.isInert(current) else { return false }
                delete(current)
                return true
            },
        ]
    }
}

/// One draft, in SentRow's envelope and metrics, laid out on two lines
/// because a popover is a third of the sent page's width.
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
        .overlay(alignment: .leading) {
            if selected && !inert {
                RoundedRectangle(cornerRadius: 1)
                    .fill(Palette.accent)
                    .frame(width: 2)
                    .padding(.vertical, 4)
            }
        }
        // The phone's way to delete, and a second way on the Mac: there is
        // no Backspace on a touch screen.
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
