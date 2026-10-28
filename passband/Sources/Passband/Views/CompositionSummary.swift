// ONE MAIL NOT YET SENT, summarized: the relation slot (`to` / `re` / `fwd`),
// who it is to or what it answers, the subject and first line, and how many
// files it holds. Shared by the two rows that describe a composition rather
// than a message: `DraftRow` (the composer's drafts page) and `ScheduledRow`
// (the outbox pinned on Sent). Only the trailing cell differs between them,
// the edit time there and the fire time and state here, so that cell is the
// caller's and everything else is this view's.
//
// Lifted out of DraftRow as it was, so a draft and a scheduled mail of the
// same words cannot read differently: the same mail pulled back from the
// outbox lands on the drafts page looking exactly like the row it left.

import SwiftUI

struct CompositionSummary<Trailing: View>: View {
    let to: String
    let subject: String
    /// The mail's words (markdown source). Not `body`, which is the view's.
    let text: String
    let replyTo: Int?
    let forwardOf: Int?
    let parentSubject: String?
    let parentFrom: String?
    let fileCount: Int
    /// The relation slot's width. The drafts page's own 26; the Sent page
    /// passes SentRow's 30 so the outbox's `to` lines up with the sent rows'.
    var slotWidth: CGFloat = 26
    /// Said on line two IN PLACE of the snippet: why a scheduled mail did not
    /// go. The subject stays, so the row still says which mail it is.
    var notice: String?
    var noticeColor: Color = Palette.inkFaint
    /// Line one of new mail with nobody in `to`. A draft really has no
    /// recipient then; an outbox row does (a fan-out, or bcc only), and says
    /// so through `ScheduledRowCopy.emptyRecipient`.
    var emptyTo = "no recipient"
    /// What stands where a sent row has its recipient's avatar: a state
    /// glyph in a tinted disc. Set only on the Sent page's outbox, so the
    /// outbox's names start on the sent rows' name line below; a drafts-page
    /// row leaves it nil and keeps its own shape.
    struct Mark {
        var symbol: String
        var tint: Color
    }
    #if os(macOS)
        /// The Mac's mark, in SentRow's 20pt avatar slot (`deskMark(_:)`).
        var deskMark: Mark?
    #else
        /// Set (by `phoneRow`) only on the Sent page's outbox, and its
        /// presence is what turns this into the phone's two-line row.
        var phoneMark: Mark?
        /// "tomorrow 9am", under the words: the half of the outbox cell that
        /// does not fit on the name line (`ScheduledRowCopy.phoneCells`).
        var phoneWhen: String?
    #endif
    @ViewBuilder let trailing: () -> Trailing

    private enum Kind { case new, reply, forward }

    private var kind: Kind {
        if forwardOf != nil { return .forward }
        return replyTo != nil ? .reply : .new
    }

    private var slot: String {
        switch kind {
        case .new: "to"
        case .reply: "re"
        case .forward: "fwd"
        }
    }

    private var names: String {
        SenderID.recipients(to)
            .map { SenderCache.resolved($0).displayName }
            .joined(separator: ", ")
    }

    /// The first line with words on it, skipping quoted mail, so a reply's
    /// snippet is what you wrote rather than what you were answering.
    private var snippet: String {
        text
            .split(whereSeparator: \.isNewline)
            .lazy
            .map { $0.trimmingCharacters(in: .whitespaces) }
            .first { !$0.isEmpty && !$0.hasPrefix(">") } ?? ""
    }

    var body: some View {
        #if os(macOS)
            line
        #else
            if let phoneMark { phoneBody(phoneMark) } else { line }
        #endif
    }

    private var line: some View {
        HStack(alignment: .firstTextBaseline, spacing: Space.scaled(9)) {
            Text(slot)
                .font(Typo.micro)
                .foregroundStyle(Palette.inkFaintest)
                .textCase(.uppercase)
                // `slotWidth` is a compact width, resolved the way SentRow's
                // `to` slot is: that is what keeps the outbox's `to` on the
                // sent rows' line in either density.
                .frame(width: MeterSlot.width(slotWidth), alignment: .trailing)

            #if os(macOS)
                if let deskMark {
                    // SentRow's avatar slot: 20pt, then the row's 9pt gap,
                    // so the outbox's names start where the sent rows' do.
                    // Centered on line one (the name), not on its baseline.
                    markDisc(deskMark, size: Self.deskMarkSize, glyph: 9)
                        .alignmentGuide(.firstTextBaseline) { d in
                            d[VerticalAlignment.center] + Typo.size(12) * 0.35
                        }
                }
            #endif

            VStack(alignment: .leading, spacing: Space.s2) {
                HStack(alignment: .firstTextBaseline, spacing: Space.s6) {
                    who
                        .frame(maxWidth: .infinity, alignment: .leading)
                    trailingCell
                }
                detail
            }
        }
    }

    /// SentRow's avatar size on the Mac. A constant there too: the avatar
    /// is an image, not room.
    static var deskMarkSize: CGFloat { 20 }

    /// The state glyph in its tinted disc, on either platform.
    private func markDisc(_ mark: Mark, size: CGFloat, glyph: CGFloat) -> some View {
        Image(systemName: mark.symbol)
            .font(Typo.ui(glyph, weight: .semibold))
            .foregroundStyle(mark.tint)
            .frame(width: size, height: size)
            .background(Circle().fill(mark.tint.opacity(0.14)))
            .accessibilityHidden(true)
    }

    #if os(macOS)
        /// The outbox row on the Mac: a state mark in the avatar slot.
        func deskMark(_ mark: Mark) -> Self {
            var row = self
            row.deskMark = mark
            return row
        }
    #endif

    // MARK: - phone

    #if !os(macOS)
        /// The outbox row as `SentRow.phoneGuts` draws a sent one, so the
        /// scheduled mail pinned above the sent mail reads as the same list:
        /// a mark, who and the stamp, then the words at the row's full width.
        /// What is its own is the mark (a state, where a sent row has a
        /// person) and a third line for the time it goes or why it did not.
        func phoneRow(mark: Mark, when: String?) -> Self {
            var row = self
            row.phoneMark = mark
            row.phoneWhen = when
            return row
        }

        private func phoneBody(_ mark: Mark) -> some View {
            HStack(alignment: .top, spacing: PhoneRow.gap) {
                markDisc(mark, size: PhoneRow.mark, glyph: 11)

                VStack(alignment: .leading, spacing: PhoneRow.lineGap) {
                    HStack(alignment: .firstTextBaseline, spacing: Space.s6) {
                        Text(slot)
                            .font(Typo.micro)
                            .foregroundStyle(Palette.inkFaintest)
                            .textCase(.uppercase)
                        who
                            .frame(maxWidth: .infinity, alignment: .leading)
                        trailingCell
                    }
                    if let words = phoneWords {
                        words
                            .lineLimit(2)
                            .multilineTextAlignment(.leading)
                            .fixedSize(horizontal: false, vertical: true)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }
                    // The notice gets a line of its own here rather than the
                    // snippet's place: there is room for both, and a mail that
                    // failed is the one whose words you want to see.
                    if phoneWhen != nil || notice != nil {
                        HStack(spacing: Space.scaled(7)) {
                            if let phoneWhen {
                                Chip(text: phoneWhen, tone: Palette.inkFaint, symbol: "clock")
                            }
                            if let notice {
                                Text(notice)
                                    .font(Typo.micro)
                                    .foregroundStyle(noticeColor)
                                    .lineLimit(1)
                            }
                        }
                        .padding(.top, 1)
                    }
                }
            }
        }

        /// Subject, then the first line of the body, as ONE `Text` so the
        /// snippet is what truncates (see `UpdateRow.summary`). Nil when
        /// there is neither, which is a reply nobody has typed into.
        private var phoneWords: Text? {
            let own = subject.trimmed
            let showSubject = !own.isEmpty || kind == .new
            let head = showSubject
                ? Text(verbatim: own.isEmpty ? "(no subject)" : own)
                    .font(Typo.rowSub)
                    .foregroundStyle(own.isEmpty ? Palette.inkFaintest : Palette.inkDim)
                : nil
            let tail = snippet.isEmpty
                ? nil
                : Text(verbatim: snippet).font(Typo.rowSub).foregroundStyle(Palette.inkFaintest)
            // The gap between them wears the row's font too: a bare literal
            // here would be body-sized and open the line spacing up.
            let gap = Text(verbatim: "  ").font(Typo.rowSub)
            switch (head, tail) {
            case let (head?, tail?): return Text("\(head)\(gap)\(tail)")
            case let (head?, nil): return head
            case let (nil, tail?): return tail
            case (nil, nil): return nil
            }
        }

        private var nameFont: Font { phoneMark == nil ? Typo.ui(12, weight: .medium) : PhoneRow.name }
    #else
        private var nameFont: Font { Typo.ui(12, weight: .medium) }
    #endif

    /// Line one: who it is to, or what it answers.
    @ViewBuilder
    private var who: some View {
        switch kind {
        case .new:
            Text(names.isEmpty ? emptyTo : names)
                .font(nameFont)
                .foregroundStyle(names.isEmpty ? Palette.inkFaintest : Palette.ink)
                .lineLimit(1)
                .truncationMode(.middle)
        case .reply, .forward:
            HStack(alignment: .firstTextBaseline, spacing: Space.scaled(5)) {
                Text(parentSubjectText)
                    .font(nameFont)
                    .foregroundStyle(Palette.ink)
                    .lineLimit(1)
                    .truncationMode(.tail)
                    .layoutPriority(1)
                if let from = parentFrom?.trimmed, !from.isEmpty {
                    Text(from)
                        .font(Typo.rowSub)
                        .foregroundStyle(Palette.inkFaintest)
                        .lineLimit(1)
                        .truncationMode(.tail)
                }
            }
        }
    }

    private var parentSubjectText: String {
        let s = parentSubject?.trimmed ?? ""
        return s.isEmpty ? "(no subject)" : s
    }

    /// Line two: the subject and the first line of the body (or the notice).
    /// A reply or forward with no subject of its own shows none: the daemon
    /// titles it from the parent, which line one already names.
    @ViewBuilder
    private var detail: some View {
        let own = subject.trimmed
        let showSubject = !own.isEmpty || kind == .new
        let tail = notice ?? snippet
        if showSubject || !tail.isEmpty {
            HStack(spacing: Space.s6) {
                if showSubject {
                    Text(own.isEmpty ? "(no subject)" : own)
                        .font(Typo.rowSub)
                        .foregroundStyle(own.isEmpty ? Palette.inkFaintest : Palette.inkDim)
                        .lineLimit(1)
                        .truncationMode(.tail)
                        .layoutPriority(1)
                }
                if !tail.isEmpty {
                    Text(tail)
                        .font(Typo.rowSub)
                        .foregroundStyle(notice == nil ? Palette.inkFaintest : noticeColor)
                        .lineLimit(1)
                        .truncationMode(.tail)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    /// Files, then the caller's cell, right-aligned on line one.
    private var trailingCell: some View {
        HStack(spacing: Space.scaled(7)) {
            if fileCount > 0 {
                HStack(spacing: Space.s2) {
                    Image(systemName: "paperclip")
                        .font(Typo.ui(9, weight: .semibold))
                    Text("\(fileCount)")
                        .font(Typo.num(10))
                }
                .foregroundStyle(Palette.inkFaint)
                .accessibilityLabel(fileCount == 1 ? "1 file" : "\(fileCount) files")
            }
            trailing()
        }
        .fixedSize()
        .layoutPriority(3)
    }
}

extension CompositionSummary {
    /// A saved draft.
    init(draft: DraftView, @ViewBuilder trailing: @escaping () -> Trailing) {
        self.init(
            to: draft.to, subject: draft.subject, text: draft.body,
            replyTo: draft.reply_to_message_id, forwardOf: draft.forward_of_message_id,
            parentSubject: draft.parent_subject, parentFrom: draft.parent_from,
            fileCount: draft.attachments?.count ?? 0, trailing: trailing)
    }

    /// A mail in the outbox.
    init(
        scheduled row: ScheduledSendView, groupName: String?, slotWidth: CGFloat,
        notice: String?, noticeColor: Color, @ViewBuilder trailing: @escaping () -> Trailing
    ) {
        self.init(
            to: row.to, subject: row.subject, text: row.body,
            replyTo: row.reply_to_message_id, forwardOf: row.forward_of_message_id,
            parentSubject: row.parent_subject, parentFrom: row.parent_from,
            fileCount: row.attachments.count, slotWidth: slotWidth, notice: notice,
            noticeColor: noticeColor,
            emptyTo: ScheduledRowCopy.emptyRecipient(groupName: groupName), trailing: trailing)
    }
}
