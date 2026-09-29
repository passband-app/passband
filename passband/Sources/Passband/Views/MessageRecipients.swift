// WHO ELSE WAS ON THIS MESSAGE — the To and Cc lines under a message's sender,
// the Bcc line on mail this account sent, and what changed since the message
// before it.
//
// The reader used to name the sender and nobody else, which cannot tell a note
// written to you apart from a broadcast you happen to be on. "Was I the only one
// asked?" and "did this go to the whole team?" are questions a thread gets opened
// with, and they were answerable here only by opening Gmail.
//
// THE TO LINE ALWAYS SHOWS. It is the answer to the question the block exists for
// — who was this written to — and hiding it behind a caret would trade one
// missing line for another. Everything under it (the Cc, the Bcc, the rest of the
// addresses) sits behind the caret at the end of that line, because printing
// eight more names over every message in a thread buries the mail underneath
// them, and in the ordinary case the answer is the same on every message anyway.
//
// WHICH IS EXACTLY WHY THE BLOCK OPENS ITSELF when the answer changes. Somebody
// added, and more to the point somebody quietly dropped, is the one thing here
// worth interrupting a read for. Additions are marked; the dropped are drawn as
// red ghosts on the header they left, because they are not recipients of this
// message and the reader still has to see them.
//
// EVERY MESSAGE CARRIES ITS OWN LINES rather than the thread carrying one set:
// that is what makes the message where the room changed the one that shows it.
//
// The grammar is `Recipients`, the same pure type the composer writes these
// headers with — see its header for why the reading and the writing side must
// not own two splitters.

import SwiftUI

extension ClientMessage {
    /// Who wrote this message and who it named, or `nil` when nobody has read its
    /// headers yet (an older daemon, or one whose recipients sweep has not walked
    /// back this far). Nil is what stops the reader claiming everyone on the next
    /// message is new.
    var audience: MessageAudience? {
        guard to_addrs != nil || cc_addrs != nil || bcc_addrs != nil else { return nil }
        return MessageAudience(
            sender: senderString,
            recipients: Recipients(
                to: to_addrs ?? "", cc: cc_addrs ?? "", bcc: bcc_addrs ?? ""))
    }
}

/// The recipient block for one message. Renders nothing at all when there is
/// nothing to say, so a thread on an older daemon reads exactly as it did.
struct MessageRecipients: View {
    let message: ClientMessage
    /// The message BEFORE this one in the thread, for the diff. `nil` on the
    /// first message and on any whose headers were never read.
    let previous: MessageAudience?
    /// The account's own address, which renders as "you" wherever it appears.
    /// `nil` when the client has not heard from `/client/stats` yet, and then
    /// every address renders as itself — a missing greeting, not a wrong one.
    let me: String?
    /// The chat style's smaller measure. Same lines, one point down, so a bubble
    /// caption does not turn into a header.
    let compact: Bool
    /// Which edge the block hangs from — `.trailing` for the user's own bubbles,
    /// which sit on the right. The lines themselves always read left to right;
    /// this is only where the block as a whole sits.
    var edge: HorizontalAlignment = .leading

    /// Open or shut, seeded from whether this message moved the room and owned
    /// per message from then on: a reader who shuts a block has answered the
    /// question it opened itself to ask.
    @State private var open: Bool

    init(
        message: ClientMessage, previous: MessageAudience?, me: String?, compact: Bool,
        edge: HorizontalAlignment = .leading
    ) {
        self.message = message
        self.previous = previous
        self.me = me
        self.compact = compact
        self.edge = edge
        // SEEDED, not bound: `changed` is a fact about the message and cannot
        // move under the reader, so the only thing that ever opens or shuts this
        // afterwards is a click. Computed here rather than in `body` so the first
        // frame is already right — a block that opened one frame late would
        // animate itself open in front of the reader.
        _open = State(
            initialValue: message.audience.map {
                Recipients.reading($0, me: me, previous: previous).changed
            } ?? false)
    }

    private var digest: RecipientDigest {
        guard let mine = message.audience else { return .none }
        return Recipients.reading(mine, me: me, previous: previous)
    }

    private var size: CGFloat { compact ? 10 : 11 }

    var body: some View {
        let digest = digest
        if let head = digest.lines.first {
            let rest = Array(digest.lines.dropFirst())
            VStack(alignment: edge, spacing: 1) {
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    RecipientLineView(line: head, size: size)
                    // NOTHING TO DROP DOWN, no control: a caret beside a To line
                    // that is already the whole answer is furniture, and the
                    // count would only restate the two names next to it.
                    if !rest.isEmpty { control(digest) }
                }
                // THE HIT TARGET IS THE FIRST LINE, not the whole block. Below it
                // the block is a list of names, and a click that lands on one of
                // those is not asking for the list to disappear.
                .contentShape(Rectangle())
                .onTapGesture { if !rest.isEmpty { open.toggle() } }
                .help(
                    rest.isEmpty || open
                        ? head.full : digest.lines.map(\.full).joined(separator: "\n"))

                if open {
                    ForEach(rest) { line in
                        RecipientLineView(line: line, size: size)
                    }
                }
            }
            .accessibilityElement(children: .combine)
            .accessibilityLabel(digest.lines.map(\.full).joined(separator: ", "))
            .accessibilityHint(
                rest.isEmpty ? "" : (open ? "hide the other recipients" : "show every recipient"))
        }
    }

    /// The caret and the count, at the end of the To line. SHUT, this is the only
    /// thing saying there is more here and how much of it; OPEN, it stays exactly
    /// where it was — a control that moves when you use it makes you find it
    /// again to undo it.
    ///
    /// The change counts ride along in both states, so a reader who shut the
    /// block can still see that this message moved the room — that is what opened
    /// it in the first place.
    private func control(_ digest: RecipientDigest) -> some View {
        var text = Text(digest.count == 1 ? "1 recipient" : "\(digest.count) recipients")
            .foregroundColor(Palette.inkFaintest)
        if digest.added > 0 {
            text = text + Text("  +\(digest.added)").foregroundColor(Palette.accent)
        }
        if digest.removed > 0 {
            text = text + Text("  −\(digest.removed)").foregroundColor(Palette.danger)
        }
        return HStack(spacing: 3) {
            Image(systemName: open ? "chevron.down" : "chevron.right")
                .font(.system(size: size - 2, weight: .semibold))
                .foregroundStyle(Palette.inkFaintest)
            text.font(.system(size: size))
        }
        .lineLimit(1)
        .fixedSize()
    }
}

private struct RecipientLineView: View {
    let line: RecipientLine
    let size: CGFloat

    /// ONE VIEW PER PERSON, flowed like a sentence, so each name is its own
    /// door to that person's contact card (click) and copy menu (right-click),
    /// exactly as the sender's name is. The line used to be one concatenated
    /// `Text`, which wrapped mid-name but had nowhere to hang a gesture per
    /// person; a flow of names wraps between people instead, which is where a
    /// list of people should break anyway.
    ///
    /// SEPARATED BY A MIDDOT, not a comma, and that is not decoration: a display
    /// name is allowed to hold a comma ("Okafor, Daniel" is one person, which is
    /// the whole reason the split is quote-aware), and joining those names back
    /// together with commas hands the reader exactly the ambiguity the parser
    /// just refused. The dot rides on the END of the name before it, so a wrap
    /// never starts a line with one.
    private var run: some View {
        FlowLayout(spacing: 0) {
            ForEach(Array(line.people.enumerated()), id: \.element.id) { i, person in
                HStack(spacing: 0) {
                    mark(person)
                        .contactCard(for: person.token)
                    if i < line.people.count - 1 {
                        Text(" · ").foregroundStyle(Palette.inkFaintest)
                    }
                }
            }
        }
    }

    private func mark(_ person: RecipientPerson) -> Text {
        switch person.mark {
        case .present:
            Text(person.name).foregroundColor(Palette.inkFaint)
        // An arrival reads as ink, not as an alarm: somebody joining a thread is
        // ordinary, and it is worth SEEING rather than worth worrying about.
        case .added:
            Text(person.name).foregroundColor(Palette.accent)
        // THE GHOST: struck through, in the danger colour. They are not on this
        // message — the strike is what says so — and they were on the one before
        // it, which is the only reason they are drawn at all.
        case .removed:
            Text(person.name).foregroundColor(Palette.danger).strikethrough()
        }
    }

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 5) {
            // Same label grammar as the sent page's "to" column: micro, upper,
            // faintest — a header rather than a word in the sentence. The width
            // is a MINIMUM and the label refuses to wrap: "bcc" is a third wider
            // than "to", and a fixed column sized for the common case broke the
            // long one across two lines.
            Text(line.label)
                .font(Typo.micro)
                .textCase(.uppercase)
                .foregroundStyle(Palette.inkFaintest)
                .lineLimit(1)
                .fixedSize()
                .frame(minWidth: 23, alignment: .trailing)
                .layoutPriority(2)

            run
                .font(.system(size: size))
        }
    }
}
