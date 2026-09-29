// One mail in the outbox, pinned above the sent mail on the Sent page: a
// sibling of SentRow, and in the drafts page's two-line shape, because it IS
// a composition rather than a message (nothing has gone out yet). The who /
// subject / snippet / files are `CompositionSummary`, the same view a draft
// row uses; only the trailing cell is this row's: when it goes and what state
// it is in, worded by `ScheduledRowCopy` (Lib/ScheduledSend.swift).
//
// The row does nothing itself. Enter / click pulls it back into the composer,
// `s` sends it now, `x` (Backspace / Delete on the Mac) cancels it behind the
// undo toast; the keys are the Sent page's (EmailsView), and the same three
// verbs sit in the context menu (and, on the phone, the swipes) so a mouse and
// a thumb reach them too.

import SwiftUI

struct ScheduledRow: View {
    let row: ScheduledSendView
    /// A fan-out's group, when this app scheduled it (`AppStore.scheduledGroupNames`).
    var groupName: String? = nil
    let selected: Bool
    let onHover: () -> Void
    let onOpen: () -> Void
    let onSendNow: () -> Void
    let onCancel: () -> Void

    /// SentRow's slot width, so the `to` here lines up with theirs below.
    private static let slotWidth: CGFloat = 30

    private var failed: Bool { row.state == "failed" }

    var body: some View {
        // onHover BEFORE onOpen, as SentRow does it: the click moves the
        // cursor to this row first.
        ListRow(
            selected: selected, cornerRadius: 8, hPadding: 11, vPadding: 6,
            onHoverChange: { if $0 { onHover() } },
            action: {
                onHover()
                // A `sending` row is in Gmail's hands: nothing to pull back.
                if row.state != "sending" { onOpen() }
            }
        ) { _, _ in
            CompositionSummary(
                scheduled: row, groupName: groupName, slotWidth: Self.slotWidth,
                notice: ScheduledRowCopy.reasonLine(row),
                noticeColor: failed ? Palette.danger : Palette.inkFaint
            ) {
                #if os(macOS)
                    // The row's own keys, on the row the keys would act on.
                    // Same form as UpdateRow's [r][e][d]. A `sending` row is
                    // in Gmail's hands and takes none of them.
                    if selected && row.state != "sending" {
                        Text("[s][x]")
                            .font(Typo.mono(9))
                            .foregroundStyle(Palette.inkFaintest)
                    }
                #endif
                // A clock, not a stamp: "in 3h" has to count down while the
                // page sits open, and nothing else re-renders this row.
                TimelineView(.periodic(from: .now, by: 30)) { context in
                    let cell = ScheduledRowCopy.trailing(row, now: context.date)
                    Text(cell.text)
                        .font(cell.tone == .quiet ? Typo.num(10) : Typo.micro)
                        .foregroundStyle(color(cell.tone))
                }
            }
        }
        .contextMenu {
            if row.state != "sending" {
                Button("edit") { onOpen() }
                Button("send now") { onSendNow() }
                Button("cancel", role: .destructive) { onCancel() }
            }
        }
        .accessibilityHint(row.state == "sending" ? "" : "opens it in the composer")
    }

    private func color(_ tone: ScheduledRowCopy.Tone) -> Color {
        switch tone {
        case .quiet: Palette.inkFaint
        case .busy: Palette.accent
        case .danger: Palette.danger
        }
    }
}
