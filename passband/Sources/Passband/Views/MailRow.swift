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

    /// The list's column widths, measured once by the list (`MailColumnsReader`)
    /// so every row draws the same sender column. Nil outside a measured list.
    @Environment(\.mailColumns) private var columns

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
            // Every fixed width and gap here is one of `MailColumns`'s, so
            // the list's fit arithmetic is the row's own.
            HStack(spacing: MailColumns.gap) {
                gutter
                    .lineLimit(1)
                    .fixedSize()
                    .frame(width: MailColumns.gutter, alignment: .leading)

                Avatar(sender: update.senderString, size: MailColumns.avatar)

                // ONE WIDTH FOR EVERY ROW, picked from the list's width rather
                // than flexed per row: a per-row width would start each subject
                // at a different x.
                Text(SenderCache.resolved(update.senderString).displayName)
                    .font(Typo.ui(12.5, weight: emphasized ? .semibold : .regular))
                    .foregroundStyle(finished ? Palette.inkFaint : Palette.ink)
                    .lineLimit(1)
                    .frame(width: columns?.name ?? MailColumns.wideName, alignment: .leading)

                // THE COLUMN THAT CARRIES THE ROW, flexible down to a small
                // floor. The floor is what the trailing marks must leave it:
                // past that they give way (`trailing`), never the row's width.
                middle(subject: subject, gist: gist)
                    .lineLimit(1)
                    .truncationMode(.tail)
                    .frame(minWidth: MailColumns.subjectFloor, maxWidth: .infinity, alignment: .leading)
                    .layoutPriority(1)

                HStack(spacing: MailColumns.markGap) {
                    trailing(selected: selected, chip: chip)
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
                        .frame(width: MailColumns.time, alignment: .trailing)
                }
                .layoutPriority(3)
            }
            // LEADING-ANCHORED. Should the fixed columns alone ever outgrow a
            // very narrow list, the row clips on the right like every other
            // row instead of being centred (each by its own overflow, which
            // staggered the meters and avatars down the list).
            .frame(minWidth: 0, maxWidth: .infinity, alignment: .leading)
        }
        // No edge bar on the selected row: ListRow's fill already marks it,
        // and a one-sided stripe on a rounded row bends into an arc. Finished
        // rows are dimmed by INK alone, never opacity, which took their faint
        // text under a readable contrast.
    }

    /// The marks by the time, in the room the subject's floor leaves them.
    /// The first arrangement that fits wins, so a row gives up the least
    /// useful marks first: the verb hint, then the reminder's words (its bell
    /// stays), then the zone pill, then everything but the deadline. Every
    /// arrangement hugs the time column, so whichever one a row ends up with,
    /// the pills' and times' right edges still line up down the list.
    ///
    /// SELECTION NEVER CHANGES THE PICK, and costs the other rows nothing.
    /// The verb hint sits OUTSIDE the fit test and only in a wide list, whose
    /// sender column was chosen with room for the full set of marks and the
    /// hint (`MailColumns.fitting`). In a narrow list it is not drawn at all:
    /// there, a hint arriving on selection could tip a borderline row into the
    /// next arrangement, and reserving its room on every row would leave a
    /// blank gap on all of them.
    @ViewBuilder
    private func trailing(selected: Bool, chip: Fmt.DeadlineChip?) -> some View {
        HStack(spacing: MailColumns.markGap) {
            // Left of the marks and pill, so a selected row's hint never
            // pushes the pill out of line with the rows around it.
            if selected, wideList, let verbHint {
                Text(verbHint)
                    .font(Typo.mono(9))
                    .foregroundStyle(Palette.inkFaintest)
                    .fixedSize()
            }
            arrangements(chip: chip)
        }
    }

    /// The list is at its usual width (or not measured yet).
    private var wideList: Bool {
        guard let columns else { return true }
        return columns.name >= MailColumns.wideName
    }

    @ViewBuilder
    private func arrangements(chip: Fmt.DeadlineChip?) -> some View {
        ViewThatFits(in: .horizontal) {
            HStack(spacing: MailColumns.markGap) {
                marks(chip: chip, reminderWords: true)
                pill
            }
            HStack(spacing: MailColumns.markGap) {
                marks(chip: chip, reminderWords: false)
                pill
            }
            HStack(spacing: MailColumns.markGap) {
                marks(chip: chip, reminderWords: false)
            }
            HStack(spacing: MailColumns.markGap) {
                deadline(chip)
            }
            // Nothing at all: a real zero-size leaf, so the last resort
            // always fits.
            Color.clear.frame(width: 0, height: 0)
        }
    }

    @ViewBuilder
    private func marks(chip: Fmt.DeadlineChip?, reminderWords: Bool) -> some View {
        if update.matched_rule != nil {
            Image(systemName: "wand.and.stars")
                .font(Typo.ui(10))
                .foregroundStyle(Palette.inkFaint)
                .help("matched a sender rule")
                .accessibilityLabel("matched a sender rule")
        }
        if update.hasPendingReminder {
            if reminderWords {
                Chip(
                    text: Fmt.remindChip(update.remind_at), tone: Palette.accent,
                    symbol: "bell.fill")
            } else {
                Image(systemName: "bell.fill")
                    .font(Typo.ui(9, weight: .semibold))
                    .foregroundStyle(Palette.accent)
                    .help(Fmt.remindChip(update.remind_at))
                    .accessibilityLabel(Fmt.remindChip(update.remind_at))
            }
        }
        deadline(chip)
        if update.hasAttachments {
            Image(systemName: "paperclip")
                .font(Typo.ui(11))
                .foregroundStyle(Palette.inkFaint)
                .accessibilityLabel("has attachments")
        }
    }

    /// Kept on finished mail too, quietly: the Deadlines lens lists it, and a
    /// row that matches with no visible reason reads as a mistake.
    ///
    /// In a NARROW list an overdue chip says just "overdue" (still filled
    /// red; the span is in its tooltip). "12 HOURS PAST DUE" is wider than
    /// the room the narrow columns keep (`MailColumns.deadlineRoom`), so at
    /// full length the most urgent mark was the first one to drop.
    @ViewBuilder
    private func deadline(_ chip: Fmt.DeadlineChip?) -> some View {
        if let chip {
            let short = chip.overdue && !wideList
            Chip(
                text: short ? "overdue" : chip.text,
                tone: finished
                    ? Palette.inkFaintest : chip.overdue ? Palette.danger : Palette.warn,
                filled: chip.overdue && !finished)
            .help(short ? chip.text : "")
        }
    }

    /// THE SITREP ZONE, as a pill HUGGING ITS TEXT and right aligned against
    /// the fixed time column, so the pills' right edges line up down the list
    /// and every point a short name does not use goes to the summary instead.
    @ViewBuilder private var pill: some View {
        if let zone = SitrepZone(update.placement) {
            PlacementPill(zone: zone, muted: finished)
        }
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

/// THE MAIL LIST'S COLUMN WIDTHS, decided ONCE from the list's width and
/// handed to every row, because a column only lines up if every row draws it
/// at the same width. A narrow list (the compose pane open beside it) takes a
/// narrower sender column; the subject flexes to its floor; the trailing marks
/// give way per row (`MailRow.trailing`) without moving any column edge.
struct MailColumns: Equatable {
    /// The sender column.
    var name: CGFloat

    /// The sender column with room to spare: the list's usual width.
    @MainActor static var wideName: CGFloat { Typo.measure(168) }
    /// What the subject keeps before the trailing marks start giving way.
    @MainActor static var subjectFloor: CGFloat { Typo.measure(64) }

    // THE ROW'S FIXED PARTS, which the row lays out with and `fitting` adds
    // up, so the arithmetic is in the row's own units: words in `measure`,
    // gaps on the spacing curve, the meter in its 10pt type's.
    /// The meter / done-check slot (`MeterSlot`: it holds 10pt type).
    @MainActor static var gutter: CGFloat { MeterSlot.width(36) }
    /// The sender's avatar. An optical size, the same in both densities.
    static let avatar: CGFloat = 20
    /// Between the row's columns.
    @MainActor static var gap: CGFloat { Space.s10 }
    /// Between the trailing marks, and between them and the time.
    @MainActor static var markGap: CGFloat { Space.scaled(7) }
    /// The time column ("Sep 28, 2025" is the widest).
    @MainActor static var time: CGFloat { Typo.measure(78) }

    /// Everything in a row but the sender column and the trailing marks, at
    /// the subject's floor: four column gaps, the mark gap the time keeps
    /// even with no marks beside it.
    @MainActor static var fixedWidth: CGFloat {
        gutter + avatar + 4 * gap + subjectFloor + markGap + time
    }

    /// A width that holds 10pt type, the marks' size (the chips and pills
    /// are `Typo.micro`): it grows as that type does.
    @MainActor private static func marks(_ compact: CGFloat) -> CGFloat {
        compact * Typo.size(10) / 10
    }

    /// THE LAST MARK'S ROOM: one deadline chip with its capsule's padding,
    /// sized for the widest a narrow list draws: an upcoming date ("due May
    /// 30", 57.6pt at 10pt) or the narrow list's short "overdue". The
    /// narrowest columns keep it, so the deadline really is the last mark a
    /// row gives up, overdue or not.
    @MainActor static var deadlineRoom: CGFloat { marks(59) + 2 * Space.scaled(7) }

    /// The sender column's floor: a short name ("Amazon.com") whole.
    @MainActor static var narrowestName: CGFloat { Typo.measure(64) }

    /// The columns for a list `width` wide. The rows' words run from the page
    /// gutter on both sides (`DeskRow.listInset` + `DeskRow.hPadding` =
    /// `Space.page`), so the row's own width is the list's less two gutters.
    ///
    /// The widest sender column that still leaves the marks their room: the
    /// full set beside the wide column, a pill and a chip beside the middle
    /// one, and past that whatever keeps a deadline, down to a floor. At the
    /// floor (the smallest window with the composer open) a comfortable row
    /// has no room left for a chip and keeps its subject instead; it never
    /// overflows (`fixedWidth + narrowestName` fits there in both densities).
    @MainActor static func fitting(listWidth width: CGFloat) -> MailColumns {
        let marksRoom = width - Space.page * 2 - fixedWidth
        if marksRoom - wideName >= marks(227) { return MailColumns(name: wideName) }
        if marksRoom - Typo.measure(128) >= marks(87) { return MailColumns(name: Typo.measure(128)) }
        return MailColumns(name: max(narrowestName, min(Typo.measure(104), marksRoom - deadlineRoom)))
    }
}

private struct MailColumnsKey: EnvironmentKey {
    static let defaultValue: MailColumns? = nil
}

extension EnvironmentValues {
    /// The mail list's measured columns, nil outside one.
    var mailColumns: MailColumns? {
        get { self[MailColumnsKey.self] }
        set { self[MailColumnsKey.self] = newValue }
    }
}

/// Measures the mail list and hands its rows `MailColumns`. A view of its own
/// so a window resize re-runs THIS body, not the page's: `content` is built
/// once by the caller and only the environment value changes, and only when
/// the list crosses a width step.
struct MailColumnsReader<Content: View>: View {
    @ViewBuilder let content: Content
    @State private var width: CGFloat = 0

    var body: some View {
        content
            .environment(\.mailColumns, width > 0 ? MailColumns.fitting(listWidth: width) : nil)
            .onGeometryChange(for: CGFloat.self) { $0.size.width } action: { width = $0 }
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
