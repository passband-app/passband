// One row of the sent page: who it went to · subject + snippet · relative time,
// plus a quiet mark when a read receipt this send armed has been fetched. One
// line on the Mac; on the phone the same four things over two (`phoneGuts`).
//
// The same envelope and the same metrics as UpdateRow, minus everything that
// would be a lie here. Outbound mail has no importance, no tier, no matched
// rule and no triage verbs, so there is no meter, no rule hint and no [r][e][d]
// hint on the selected row — a key hint for a key that does nothing is worse
// than no hint at all.

import SwiftUI

struct SentRow: View {
    let item: SentItem
    let selected: Bool
    let onHover: () -> Void
    let onOpen: () -> Void

    /// The recipient list, parsed once per render pass and read twice below.
    private var to: [String] { SenderID.recipients(item.to) }

    /// Recipients as people rather than as a header: "Sarah Chen, billing@acme".
    /// Truncated in the MIDDLE, because the ends of a long list are the halves
    /// that identify it — who you wrote to first, and who is on the tail.
    private var names: String {
        to.map { SenderCache.resolved($0).displayName }.joined(separator: ", ")
    }

    var body: some View {
        // onHover BEFORE onOpen, exactly as UpdateRow does it: the click moves
        // the cursor to this row first, so the keyboard picks up where the
        // pointer left it.
        ListRow(
            selected: selected, cornerRadius: Self.radius, hPadding: Self.hPadding,
            vPadding: Self.vPadding,
            onHoverChange: { if $0 { onHover() } },
            action: {
                onHover()
                onOpen()
            }
        ) { _, _ in
            // Neither flag is read: the guts of a sent row are the same
            // selected or not, because there are no verb hints to reveal.
            #if os(macOS)
                desktopGuts
            #else
                phoneGuts
            #endif
        }
        .overlay(alignment: .leading) {
            if selected {
                RoundedRectangle(cornerRadius: 1)
                    .fill(Palette.accent)
                    .frame(width: 2)
                    .padding(.vertical, Space.s4)
            }
        }
    }

    /// The inbox rows' own numbers on either platform, so the page under the
    /// segmented control keeps its rhythm when it flips.
    #if os(macOS)
        private static var hPadding: CGFloat { DeskRow.hPadding }
        private static var vPadding: CGFloat { DeskRow.vPadding }
        private static let radius: CGFloat = Radius.row
    #else
        private static var hPadding: CGFloat { PhoneRow.hPadding }
        private static var vPadding: CGFloat { PhoneRow.vPadding }
        private static let radius: CGFloat = PhoneRow.radius
    #endif

    /// The read receipt, kept deliberately quiet: an open is a pixel fetch and
    /// sometimes a proxy warming its cache, so the row states the count and
    /// nothing louder. No opens means no mark: "not opened" and "never
    /// tracked" are the same silence (see ReadReceipt.swift).
    @ViewBuilder private var opens: some View {
        if item.opens > 0 {
            HStack(spacing: Space.scaled(3)) {
                Image(systemName: "eye")
                    .font(Typo.ui(9, weight: .semibold))
                Text("\(item.opens)")
                    .font(Typo.num(10))
            }
            .foregroundStyle(Palette.inkFaint)
            .help(item.opens == 1 ? "opened once" : "opened \(item.opens) times")
            .accessibilityLabel(
                item.opens == 1 ? "opened once" : "opened \(item.opens) times")
        }
    }

    // MARK: - phone

    /// ONE ROW, TWO LINES, the shape `UpdateRow` takes on a phone and for the
    /// same reason. The Mac's single line gives the recipient a fixed column
    /// and the subject whatever is left, and on a 393pt screen what is left
    /// was about ten characters. So who it went to and when take the top
    /// line, and the subject gets the width of the row, twice, with the
    /// snippet running on behind it in fainter ink.
    #if !os(macOS)
        /// ONE `Text`, so the pair truncates in one place and the snippet is
        /// eaten before the subject is; see `UpdateRow.summary`. Verbatim for
        /// the same reason as there: these are a mail's words, not a key.
        private var words: Text {
            let subject = Text(verbatim: item.subject.isEmpty ? "(no subject)" : item.subject)
                .font(Typo.rowSub)
                .foregroundStyle(item.subject.isEmpty ? Palette.inkFaintest : Palette.inkDim)
            guard !item.snippet.isEmpty else { return subject }
            let snippet = Text(verbatim: "  \(item.snippet)")
                .font(Typo.rowSub)
                .foregroundStyle(Palette.inkFaintest)
            return Text("\(subject)\(snippet)")
        }

        private var phoneGuts: some View {
            HStack(alignment: .top, spacing: PhoneRow.gap) {
                // The slot is held when the header carried nothing usable, as
                // on the Mac, so a recipient-less row keeps the text edge.
                Group {
                    if let first = to.first {
                        Avatar(sender: first, size: PhoneRow.mark)
                    } else {
                        Circle().fill(Palette.hairline)
                    }
                }
                .frame(width: PhoneRow.mark, height: PhoneRow.mark)

                VStack(alignment: .leading, spacing: PhoneRow.lineGap) {
                    HStack(alignment: .firstTextBaseline, spacing: Space.s6) {
                        // Still said: the name here is a RECIPIENT, in the
                        // position every other list on this tab puts a sender.
                        Text("to")
                            .font(Typo.micro)
                            .foregroundStyle(Palette.inkFaintest)
                            .textCase(.uppercase)
                        Text(names.isEmpty ? "no recipient" : names)
                            .font(PhoneRow.name)
                            .foregroundStyle(names.isEmpty ? Palette.inkFaintest : Palette.ink)
                            .lineLimit(1)
                            .truncationMode(.middle)
                        Spacer(minLength: Space.s4)
                        opens
                        Text(Fmt.relAge(item.sent_at))
                            .font(PhoneRow.stamp)
                            .foregroundStyle(Palette.inkFaintest)
                    }

                    words
                        .lineLimit(2)
                        .multilineTextAlignment(.leading)
                        .fixedSize(horizontal: false, vertical: true)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
            }
        }
    #endif

    // MARK: - desktop

    #if os(macOS)
        private var desktopGuts: some View {
            HStack(spacing: Space.scaled(9)) {
                // Stands in for UpdateRow's importance meter, in a slot the same
                // width (five mono glyphs at 10pt) so flipping pages under the
                // segmented control does not shunt the avatar and name column
                // sideways. It earns the space: sent rows name a RECIPIENT, and
                // an unlabelled name here would read as the sender every other
                // list puts in this position.
                Text("to")
                    .font(Typo.micro)
                    .foregroundStyle(Palette.inkFaintest)
                    .textCase(.uppercase)
                    .frame(width: MeterSlot.width(), alignment: .trailing)
                    .layoutPriority(2)

                // The slot is held even when the header carried nothing usable,
                // so a recipient-less row does not shunt the whole list left.
                Group {
                    if let first = to.first {
                        Avatar(sender: first, size: 20)
                    } else {
                        Circle().fill(Palette.hairline)
                    }
                }
                .frame(width: 20, height: 20)

                Text(names.isEmpty ? "no recipient" : names)
                    .font(Typo.ui(12, weight: .medium))
                    .foregroundStyle(names.isEmpty ? Palette.inkFaintest : Palette.ink)
                    .lineLimit(1)
                    .truncationMode(.middle)
                    .frame(
                        minWidth: Typo.measure(96), idealWidth: Typo.measure(150),
                        maxWidth: Typo.measure(190), alignment: .leading)
                    .layoutPriority(2)

                // Subject first and at priority, snippet in whatever is left:
                // the subject is what a sent row is looked up by, and the
                // snippet is the reminder of which draft it was.
                HStack(spacing: Space.s6) {
                    Text(item.subject.isEmpty ? "(no subject)" : item.subject)
                        .font(Typo.rowSub)
                        .foregroundStyle(Palette.inkDim)
                        .lineLimit(1)
                        .truncationMode(.tail)
                        .layoutPriority(1)
                    if !item.snippet.isEmpty {
                        Text(item.snippet)
                            .font(Typo.rowSub)
                            .foregroundStyle(Palette.inkFaintest)
                            .lineLimit(1)
                            .truncationMode(.tail)
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)

                HStack(spacing: Space.scaled(7)) {
                    opens
                    Text(Fmt.relAge(item.sent_at))
                        .font(Typo.num(10))
                        .foregroundStyle(Palette.inkFaintest)
                        .frame(width: MeterSlot.width(), alignment: .trailing)
                }
                .layoutPriority(3)
            }
            // Leading-anchored, like MailRow: a row that outgrows a very
            // narrow list clips on the right rather than centring.
            .frame(minWidth: 0, maxWidth: .infinity, alignment: .leading)
        }
    #endif
}
