// WHO ELSE WAS ON THIS MESSAGE — the To and Cc lines under a message's sender,
// and the Bcc line on mail this account sent.
//
// The reader used to name the sender and nobody else, which cannot tell a note
// written to you apart from a broadcast you happen to be on. "Was I the only one
// asked?" and "did this go to the whole team?" are questions a thread gets opened
// with, and they were answerable here only by opening Gmail.
//
// EVERY MESSAGE CARRIES ITS OWN LINES rather than the thread carrying one set:
// people are added and dropped mid-thread, and the message where that happened
// is the one worth seeing it on.
//
// The grammar is `Recipients`, the same pure type the composer writes these
// headers with — see its header for why the reading and the writing side must
// not own two splitters.

import SwiftUI

/// The recipient lines for one message. Renders nothing at all when the daemon
/// sent no recipients (too old to know about them, or its one-shot sweep has not
/// walked back to this row yet) and nothing when the headers named nobody.
struct MessageRecipients: View {
    let message: ClientMessage
    /// The account's own address, which renders as "you" wherever it appears.
    /// `nil` when the client has not heard from `/client/stats` yet, and then
    /// every address renders as itself — a missing greeting, not a wrong one.
    let me: String?
    /// The chat style's smaller measure. Same lines, one point down, so a bubble
    /// caption does not turn into a header.
    let compact: Bool

    /// Expanded by a click on "+N more", and it opens ALL THREE lines at once:
    /// the question behind the click is "who is on this", not "who is on the Cc".
    @State private var expanded = false

    /// How many names a line shows before the rest go behind the counter.
    ///
    /// Not truncation — every name is one click away, and the full list with
    /// addresses is in the tooltip either way. A list past this length is a
    /// mailing list or a fan-out, and forty names printed over every message in
    /// a thread bury the mail underneath them.
    private static let inlineCap = 6

    private var lines: [RecipientLine] {
        Recipients.reading(
            to: message.to_addrs, cc: message.cc_addrs, bcc: message.bcc_addrs, me: me)
    }

    var body: some View {
        // The whole block is ABSENT, not empty, when there is nothing to say —
        // no reserved gutter, no stray padding under a header, and an older
        // daemon's threads read exactly as they did before.
        let lines = lines
        if !lines.isEmpty {
            VStack(alignment: .leading, spacing: 1) {
                ForEach(lines) { line in
                    RecipientLineView(
                        line: line, compact: compact,
                        cap: expanded ? Int.max : Self.inlineCap,
                        onExpand: { expanded = true })
                }
            }
        }
    }
}

private struct RecipientLineView: View {
    let line: RecipientLine
    let compact: Bool
    let cap: Int
    let onExpand: () -> Void

    private var hidden: Int { max(0, line.names.count - cap) }
    private var size: CGFloat { compact ? 10 : 11 }

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 5) {
            // Same label grammar as the sent page's "to" column: micro, upper,
            // faintest — a header rather than a word in the sentence.
            Text(line.label)
                .font(Typo.micro)
                .textCase(.uppercase)
                .foregroundStyle(Palette.inkFaintest)
                .frame(width: 21, alignment: .trailing)
                .layoutPriority(2)

            // ONE Text, not a stack of them: the names wrap mid-list the way a
            // sentence does, and a flow of chips here would take attention the
            // message body has more claim to.
            Text(line.names.prefix(cap).joined(separator: ", "))
                .font(.system(size: size))
                .foregroundStyle(Palette.inkFaint)
                .fixedSize(horizontal: false, vertical: true)

            if hidden > 0 {
                Button(action: onExpand) {
                    Text("+\(hidden) more")
                        .font(.system(size: size, weight: .medium))
                        .foregroundStyle(Palette.inkFaintest)
                }
                .buttonStyle(.plain)
                .help("show every recipient")
            }
            Spacer(minLength: 0)
        }
        .help(line.full)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(line.full)
    }
}
