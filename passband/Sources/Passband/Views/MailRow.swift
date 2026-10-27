// ONE ROW OF THE ALL-MAIL LIST, laid out the way thirty years of mail clients
// taught people to read one: who, what, when. Sender in a fixed column, then the
// mail's own subject, then — where a mail client would print the opening of the
// body — the triage one-liner, which says what the body would have taken a
// paragraph to. Time on the right edge, as a time today and a date before that.
//
// The sitrep's judgement rides along rather than being left behind: mail triage
// picked for you sets its sender and subject in the heavier weight a list
// reader already knows as "look at this one", the importance meter stays in the
// gutter, deadline / reminder / rule marks sit by the time, and a tinted pill
// names the sitrep zone the mail was filed under.
//
// `UpdateRow` stays the row for the sitrep's bands and for the phone, where the
// two-line summary-first layout is the right one.

import SwiftUI

struct MailRow: View {
    let update: AttentionUpdate
    let selected: Bool
    /// Finished mail: dimmed and checked rather than hidden, so it reads as
    /// handled instead of as missing.
    var done = false
    /// The selected row's key hint, or nil for none. Only the spam page
    /// passes one (its `i` has no other affordance on the Mac).
    var verbHint: String? = nil
    let onHover: () -> Void
    let onOpen: () -> Void

    /// The mail's own subject, when the daemon sent one that says something
    /// the one-liner does not. A summary that IS the subject (an untriaged
    /// row's fallback) would otherwise print twice.
    private static func subject(_ update: AttentionUpdate) -> String? {
        guard let s = update.subject?.displaySubject.flattenedLine(cap: 300), !s.isEmpty
        else { return nil }
        return s
    }

    /// What follows the subject: the one-liner, or for a row nothing triaged,
    /// the opening of its body.
    private static func gist(_ update: AttentionUpdate, subject: String?) -> String {
        let line = update.one_line.flattenedLine(cap: 300)
        if line.isEmpty { return (update.preview ?? "").flattenedLine(cap: 300) }
        return line == subject ? "" : line
    }

    /// Done and NOT parked. Parking resolves the thread too, but a parked row
    /// is coming back, and its reminder chip says when; drawing it finished
    /// (dimmed, checked) would say the opposite.
    private var finished: Bool { done && !update.hasPendingReminder }

    /// Picked for attention and not yet finished: the rows a list reader's eye
    /// should land on first. The same test as the Important lens.
    private var emphasized: Bool { MailFilter.important.matches(update) && !finished }

    /// The middle column as ONE `Text`, so it truncates in one place and in
    /// the right order: the gist is eaten first, the subject last.
    /// `verbatim` for both, since neither is ours to localize.
    ///
    /// The subject and the gist are worked out ONCE a body and passed in:
    /// they were computed properties, and this column alone asked for the
    /// subject four times, each a trim, a cap and a flatten of the line.
    private func middle(subject: String?, gist: String) -> Text {
        let weight: Font.Weight = emphasized ? .semibold : .regular
        let gistText = Text(verbatim: gist)
            .font(Typo.rowSub)
            .foregroundStyle(Palette.inkFaint)
        guard let subject else {
            // No subject on the wire (an older daemon, a rehearsal row): the
            // one-liner takes the subject's place and its ink.
            return Text(verbatim: gist)
                .font(Typo.ui(12.5, weight: weight))
                .foregroundStyle(finished ? Palette.inkFaint : Palette.ink)
        }
        let head = Text(verbatim: subject)
            .font(Typo.ui(12.5, weight: weight))
            .foregroundStyle(finished ? Palette.inkFaint : Palette.ink)
        guard !gist.isEmpty else { return head }
        // Three spaces and a drop in ink, no glyph: the weight and colour change
        // already say where the subject ends.
        return Text("\(head)\(Text(verbatim: "   "))\(gistText)")
    }


    var body: some View {
        let chip = Fmt.deadlineChip(update.displayDeadline)
        let subject = Self.subject(update)
        let gist = Self.gist(update, subject: subject)
        ListRow(
            selected: selected, cornerRadius: Radius.row,
            hPadding: DeskRow.hPadding, vPadding: DeskRow.vPadding,
            onHoverChange: { if $0 { onHover() } },
            action: {
                onHover()
                onOpen()
            }
        ) { selected, _ in
            HStack(spacing: Space.s10) {
                gutter
                    .lineLimit(1)
                    .fixedSize()
                    .frame(width: 36, alignment: .leading)

                Avatar(sender: update.senderString, size: 20)

                Text(SenderCache.resolved(update.senderString).displayName)
                    .font(Typo.ui(12.5, weight: emphasized ? .semibold : .regular))
                    .foregroundStyle(finished ? Palette.inkFaint : Palette.ink)
                    .lineLimit(1)
                    .frame(width: Typo.measure(168), alignment: .leading)

                // THE COLUMN THAT CARRIES THE ROW, so it is the last to give
                // way: a floor and a priority over the chips beside it, which
                // otherwise ate it whole in a narrow window.
                middle(subject: subject, gist: gist)
                    .lineLimit(1)
                    .truncationMode(.tail)
                    .frame(minWidth: Typo.measure(140), maxWidth: .infinity, alignment: .leading)
                    .layoutPriority(1)

                HStack(spacing: Space.scaled(7)) {
                    if update.matched_rule != nil {
                        Image(systemName: "wand.and.stars")
                            .font(Typo.ui(10))
                            .foregroundStyle(Palette.inkFaint)
                            .help("matched a sender rule")
                            .accessibilityLabel("matched a sender rule")
                    }
                    if update.hasPendingReminder {
                        Chip(
                            text: Fmt.remindChip(update.remind_at), tone: Palette.accent,
                            symbol: "bell.fill")
                    }
                    // Kept on finished mail too, quietly: the Deadlines lens
                    // lists it, and a row that matches with no visible reason
                    // reads as a mistake.
                    if let chip {
                        Chip(
                            text: chip.text,
                            tone: finished
                                ? Palette.inkFaintest : chip.overdue ? Palette.danger : Palette.warn,
                            filled: chip.overdue && !finished)
                    }
                    if update.hasAttachments {
                        Image(systemName: "paperclip")
                            .font(Typo.ui(11))
                            .foregroundStyle(Palette.inkFaint)
                            .accessibilityLabel("has attachments")
                    }
                    // Left of the pill, so a selected row's hint never pushes
                    // the pill out of line with the rows around it.
                    if selected, let verbHint {
                        Text(verbHint)
                            .font(Typo.mono(9))
                            .foregroundStyle(Palette.inkFaintest)
                    }
                    // THE SITREP ZONE, as a pill HUGGING ITS TEXT and right
                    // aligned against the fixed time column, so the pills'
                    // right edges line up down the list and every point a
                    // short name does not use goes to the summary instead.
                    if let zone = SitrepZone(update.placement) {
                        PlacementPill(zone: zone, muted: finished)
                    }
                    // ARRIVAL ONLY. A daemon that does not send `received_at`
                    // has no arrival to print, and its listing time (every
                    // backfilled row reading "11:41 AM") is worse than none.
                    // ALWAYS the fixed column, even empty: it is the edge the
                    // pills line up against ("Sep 28, 2025" is the widest).
                    // Never bold: the sender and subject already carry it.
                    Text(MailList.stamp(update.received_at))
                        .font(Typo.ui(11.5))
                        .monospacedDigit()
                        .foregroundStyle(emphasized ? Palette.inkDim : Palette.inkFaint)
                        .frame(width: Typo.measure(78), alignment: .trailing)
                }
                .layoutPriority(3)
            }
        }
        // No edge bar on the selected row: ListRow's fill already marks it,
        // and a one-sided stripe on a rounded row bends into an arc. Finished
        // rows are dimmed by INK alone, never opacity, which took their faint
        // text under a readable contrast.
    }

    /// The left gutter: a check on finished mail, the importance meter on the
    /// rest. One slot, so the sender column never moves.
    @ViewBuilder private var gutter: some View {
        if finished {
            Image(systemName: "checkmark")
                .font(Typo.ui(10, weight: .semibold))
                .foregroundStyle(Palette.positive)
                .accessibilityLabel("done")
        } else {
            Text(Fmt.importanceMeter(update.importance))
                .font(Typo.mono(10))
                .foregroundStyle(Palette.importanceColor(update.importance))
                .accessibilityLabel(
                    update.importance >= 70
                        ? "high importance" : update.importance >= 40 ? "medium importance" : "low importance")
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
        case .auth: "key"
        case .calendar: "calendar"
        case .shipments: "shippingbox"
        case .billing: "building.columns"
        case .receipts: "receipt"
        case .reading: "envelope.open"
        case .noise: "speaker.slash"
        }
    }

    /// The sitrep header's colour for the zone: the pill's wash.
    var tint: Color {
        switch self {
        case .eyes, .calendar: Palette.accent
        case .auth: Palette.lock
        case .shipments: Palette.warn
        case .billing: Palette.accentInk
        case .receipts: Palette.positive
        case .reading: Color(light: Color(hex: 0xC9A15A), dark: Color(hex: 0xE8CF9E))
        case .noise: Palette.inkFaint
        }
    }

    /// The pill's TEXT colour: the tint, deepened in light mode, where 10pt
    /// text in the raw tint on its own 15% wash falls under 4.5:1.
    var ink: Color {
        switch self {
        case .eyes, .calendar: Color(light: Color(hex: 0x1C5FA8), dark: Color(hex: 0x6FB2F2))
        case .auth: Color(light: Color(hex: 0x4A3BB5), dark: Color(hex: 0x9B8CF5))
        case .shipments: Color(light: Color(hex: 0x8A5A12), dark: Color(hex: 0xE8AC4C))
        case .billing: Palette.accentInk
        case .receipts: Color(light: Color(hex: 0x1A6642), dark: Color(hex: 0x4FC08A))
        case .reading: Color(light: Color(hex: 0x7A5A1E), dark: Color(hex: 0xE8CF9E))
        case .noise: Palette.inkFaint
        }
    }
}

/// The zone as a tinted pill: its icon and name on a wash of its colour, the
/// whole capsule filled evenly (never one lit edge). Noise and finished rows
/// are grey: a done row's loudest mark should not be its category.
struct PlacementPill: View {
    let zone: SitrepZone
    var muted = false

    var body: some View {
        let grey = muted || zone == .noise
        HStack(spacing: Space.s4) {
            Image(systemName: zone.symbol)
                .font(Typo.ui(9, weight: .semibold))
            Text(zone.title)
                .font(Typo.micro)
                .lineLimit(1)
                .fixedSize()
        }
        .foregroundStyle(grey ? Palette.inkFaint : zone.ink)
        .padding(.horizontal, Space.scaled(7))
        .padding(.vertical, Space.scaled(2.5))
        .background(Capsule().fill((grey ? Palette.inkFaint : zone.tint).opacity(grey ? 0.10 : 0.15)))
        .accessibilityElement(children: .combine)
        .accessibilityLabel("filed under \(zone.title)")
    }
}
