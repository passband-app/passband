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
        // Three spaces and a drop in ink, no glyph: the weight and colour change
        // already say where the subject ends.
        return Text("\(head)\(Text(verbatim: "   "))\(gistText)")
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
                    // Left of the pill, so a selected row's hint never pushes
                    // the pill out of line with the rows around it.
                    if selected {
                        Text(verbHint)
                            .font(Typo.mono(9))
                            .foregroundStyle(Palette.inkFaintest)
                    }
                    // THE SITREP ZONE, as a pill HUGGING ITS TEXT and right
                    // aligned against the fixed time column, so the pills'
                    // right edges line up down the list and every point a
                    // short name does not use goes to the summary instead.
                    if let zone = SitrepZone(update.placement) {
                        PlacementPill(zone: zone)
                    }
                    // ARRIVAL ONLY. A daemon that does not send `received_at`
                    // has no arrival to print, and its listing time (every
                    // backfilled row reading "11:41 AM") is worse than none.
                    let stamp = MailList.stamp(update.received_at)
                    if !stamp.isEmpty {
                        Text(stamp)
                            .font(.system(size: 11.5, weight: emphasized ? .semibold : .regular))
                            .monospacedDigit()
                            .foregroundStyle(emphasized ? Palette.inkDim : Palette.inkFaint)
                            // FIXED, not a minimum: it is the edge the pills
                            // line up against ("Sep 28, 2025" is the widest).
                            .frame(width: 76, alignment: .trailing)
                    }
                }
                .layoutPriority(3)
            }
        }
        // No edge bar on the selected row: ListRow's fill already marks it,
        // and a one-sided stripe on a rounded row bends into an arc.
        // Dimmed mostly by INK rather than opacity: the words step down and
        // the avatar keeps its colour, which reads as handled, not disabled.
        .opacity(done ? 0.8 : 1)
    }

    /// The left gutter: a check on finished mail, the importance meter on the
    /// rest. One slot, so the sender column never moves.
    @ViewBuilder private var gutter: some View {
        if update.hasPendingReminder {
            // Parked, not finished: it is coming back.
            Image(systemName: "bell")
                .font(.system(size: 10, weight: .semibold))
                .foregroundStyle(Palette.accent)
                .accessibilityLabel("reminder set")
        } else if done {
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

/// The sitrep zone a row lives in, by the daemon's `placement`. Same symbols
/// and names as the sitrep's own zone headers, so the label points at a place
/// the user has seen.
enum SitrepZone: String, CaseIterable {
    // The raw values ARE the wire's `placement` strings.
    case eyes, auth, calendar, shipments, billing, receipts, reading, noise

    init?(_ wire: String?) {
        guard let wire, let zone = SitrepZone(rawValue: wire) else { return nil }
        self = zone
    }

    var title: String {
        switch self {
        case .eyes: "For your eyes"
        case .auth: "Auth"
        case .calendar: "Calendar"
        case .shipments: "Shipments"
        case .billing: "Billing"
        case .receipts: "Receipts"
        case .reading: "Reading"
        case .noise: "Noise"
        }
    }

    var symbol: String {
        switch self {
        case .eyes: "eye"
        case .auth: "lock"
        case .calendar: "calendar"
        case .shipments: "shippingbox"
        case .billing: "building.columns"
        case .receipts: "receipt"
        case .reading: "envelope.open"
        case .noise: "speaker.slash"
        }
    }

    var tint: Color {
        switch self {
        case .eyes, .calendar: Palette.accent
        case .auth: Palette.lock
        case .shipments: Palette.warn
        case .billing, .receipts: Palette.positive
        case .reading: Palette.inkDim
        case .noise: Palette.inkFaintest
        }
    }
}

/// The zone as a tinted pill: its icon and name on a wash of its colour, the
/// whole capsule filled evenly (never one lit edge). Noise is a neutral grey,
/// the one zone that should not catch the eye.
struct PlacementPill: View {
    let zone: SitrepZone

    var body: some View {
        let tint = zone == .noise ? Palette.inkFaint : zone.tint
        HStack(spacing: 4) {
            Image(systemName: zone.symbol)
                .font(.system(size: 9, weight: .semibold))
            Text(zone.title)
                .font(Typo.micro)
                .lineLimit(1)
                .fixedSize()
        }
        .foregroundStyle(tint)
        .padding(.horizontal, 7)
        .padding(.vertical, 2.5)
        .background(Capsule().fill(tint.opacity(zone == .noise ? 0.10 : 0.15)))
        .accessibilityElement(children: .combine)
        .accessibilityLabel("filed under \(zone.title)")
    }
}
