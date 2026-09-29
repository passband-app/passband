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
        HStack(alignment: .firstTextBaseline, spacing: 9) {
            Text(slot)
                .font(Typo.micro)
                .foregroundStyle(Palette.inkFaintest)
                .textCase(.uppercase)
                .frame(width: slotWidth, alignment: .trailing)

            VStack(alignment: .leading, spacing: 2) {
                HStack(alignment: .firstTextBaseline, spacing: 6) {
                    who
                        .frame(maxWidth: .infinity, alignment: .leading)
                    trailingCell
                }
                detail
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
                Text(parentSubjectText)
                    .font(.system(size: 12, weight: .medium))
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
            HStack(spacing: 6) {
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
        scheduled row: ScheduledSendView, slotWidth: CGFloat, notice: String?,
        noticeColor: Color, @ViewBuilder trailing: @escaping () -> Trailing
    ) {
        self.init(
            to: row.to, subject: row.subject, text: row.body,
            replyTo: row.reply_to_message_id, forwardOf: row.forward_of_message_id,
            parentSubject: row.parent_subject, parentFrom: row.parent_from,
            fileCount: row.attachments.count, slotWidth: slotWidth, notice: notice,
            noticeColor: noticeColor, trailing: trailing)
    }
}
