// ONE ROW OF THE ALL-MAIL LIST, laid out the way thirty years of mail clients
// taught people to read one: who, what, when. Sender in a fixed column, then the
// mail's own subject, then — where a mail client would print the opening of the
// body — the triage one-liner, which says what the body would have taken a
// paragraph to. Time on the right edge, as a time today and a date before that.
//
// The sitrep's judgement rides along rather than being left behind: mail triage
// picked for you sets its sender and subject in the heavier weight a list
// reader already knows as "look at this one", the importance meter stays in the
// gutter, deadline / reminder / rule chips sit by the time, and hovering the
// summary shows why triage filed it where it did.
//
// `UpdateRow` stays the row for the sitrep's bands and for the phone, where the
// two-line summary-first layout is the right one.

import SwiftUI

struct MailRow: View {
    let update: AttentionUpdate
    let selected: Bool
    /// Finished mail, shown on "everything": dimmed and checked rather than
    /// hidden, so it reads as handled instead of as missing.
    var done = false
    var verbHint: String = "[r][e][d]"
    let onHover: () -> Void
    let onOpen: () -> Void

    /// Picked for attention and not yet finished: the rows a list reader's eye
    /// should land on first.
    private var emphasized: Bool { update.tier != .noise && !done }

    /// The mail's own subject, when the daemon sent one that says something
    /// the one-liner does not. A summary that IS the subject (an untriaged
    /// row's fallback) would otherwise print twice.
    private var subject: String? {
        guard let s = update.subject?.displaySubject.flattenedLine(cap: 300), !s.isEmpty
        else { return nil }
        return s
    }

    /// What follows the subject: the one-liner, or for a row nothing triaged,
    /// the opening of its body.
    private var gist: String {
        let line = update.one_line.flattenedLine(cap: 300)
        if !line.isEmpty, line != subject { return line }
        if line.isEmpty { return (update.preview ?? "").flattenedLine(cap: 300) }
        return ""
    }

    /// The middle column as ONE `Text`, so it truncates in one place and in
    /// the right order: the gist is eaten first, the subject last.
    /// `verbatim` for both, since neither is ours to localize.
    private var middle: Text {
        let weight: Font.Weight = emphasized ? .semibold : .regular
        let gistText = Text(verbatim: gist)
            .font(Typo.rowSub)
            .foregroundStyle(done ? Palette.inkFaintest : Palette.inkFaint)
        guard let subject else {
            // No subject on the wire (an older daemon, a rehearsal row): the
            // one-liner takes the subject's place and its ink.
            return Text(verbatim: gist)
                .font(.system(size: 12.5, weight: weight))
                .foregroundStyle(done ? Palette.inkFaint : Palette.ink)
        }
        let head = Text(verbatim: subject)
            .font(.system(size: 12.5, weight: weight))
            .foregroundStyle(done ? Palette.inkFaint : Palette.ink)
        guard !gist.isEmpty else { return head }
        // Two spaces and a drop in ink, no glyph: the weight and colour change
        // already say where the subject ends.
        return Text("\(head)\(Text(verbatim: "   "))\(gistText)")
    }

    /// The triage's own reason, offered on hover. Hidden when it is the
    /// placeholder an untriaged row carries.
    private var why: String? {
        let r = update.reason.trimmingCharacters(in: .whitespacesAndNewlines)
        return r.isEmpty || r == "Triage pending" ? nil : "Why it's here: \(r)"
    }

    var body: some View {
        let chip = Fmt.deadlineChip(update.displayDeadline)
        ListRow(
            selected: selected, cornerRadius: 8, hPadding: 11, vPadding: 7,
            onHoverChange: { if $0 { onHover() } },
            action: {
                onHover()
                onOpen()
            }
        ) { selected, _ in
            HStack(spacing: 10) {
                gutter
                    .lineLimit(1)
                    .fixedSize()
                    .frame(width: 36, alignment: .leading)

                Avatar(sender: update.senderString, size: 20)

                Text(SenderCache.resolved(update.senderString).displayName)
                    .font(.system(size: 12.5, weight: emphasized ? .semibold : .regular))
                    .foregroundStyle(done ? Palette.inkFaint : Palette.ink)
                    .lineLimit(1)
                    .frame(width: 168, alignment: .leading)

                middle
                    .lineLimit(1)
                    .truncationMode(.tail)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .help(why ?? "")

                HStack(spacing: 7) {
                    if update.matched_rule != nil {
                        Text("·rule")
                            .font(Typo.micro)
                            .foregroundStyle(Palette.accent.opacity(0.8))
                            .help("matched a sender rule")
                    }
                    if update.hasPendingReminder {
                        Chip(
                            text: Fmt.remindChip(update.remind_at), tone: Palette.accent,
                            symbol: "bell.fill")
                    }
                    if let chip, !done {
                        Chip(
                            text: chip.text, tone: chip.overdue ? Palette.danger : Palette.warn,
                            filled: chip.overdue)
                    }
                    if update.hasAttachments {
                        Image(systemName: "paperclip")
                            .font(.system(size: 11))
                            .foregroundStyle(Palette.inkFaint)
                            .accessibilityLabel("has attachments")
                    }
                    if selected {
                        Text(verbHint)
                            .font(Typo.mono(9))
                            .foregroundStyle(Palette.inkFaintest)
                    }
                    Text(MailList.stamp(Fmt.date(update.arrivedAt)))
                        .font(.system(size: 11.5, weight: emphasized ? .semibold : .regular))
                        .monospacedDigit()
                        .foregroundStyle(emphasized ? Palette.inkDim : Palette.inkFaint)
                        .frame(minWidth: 52, alignment: .trailing)
                }
                .layoutPriority(3)
            }
        }
        // No edge bar on the selected row: ListRow's fill already marks it,
        // and a one-sided stripe on a rounded row bends into an arc.
        // Dimmed mostly by INK rather than opacity: the words step down and
        // the avatar keeps its colour, which reads as handled, not disabled.
        .opacity(done ? 0.8 : 1)
        .accessibilityElement(children: .combine)
        .accessibilityValue(done ? "done" : "")
    }

    /// The left gutter: a check on finished mail, the importance meter on the
    /// rest. One slot, so the sender column never moves.
    @ViewBuilder private var gutter: some View {
        if done {
            Image(systemName: "checkmark")
                .font(.system(size: 10, weight: .semibold))
                .foregroundStyle(Palette.positive)
                .accessibilityLabel("done")
        } else {
            Text(Fmt.importanceMeter(update.importance))
                .font(Typo.mono(10))
                .foregroundStyle(Palette.importanceColor(update.importance))
                .accessibilityLabel("importance \(update.importance)")
        }
    }
}
