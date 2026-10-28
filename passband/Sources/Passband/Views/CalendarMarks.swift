// THE CALENDAR'S MARKS, shared by the Mac's page (Views/CalendarPage.swift and
// its month and week) and the phone's tab (PassbandiOS/Views/
// MobileCalendarView.swift): the account's hue, each calendar's inks, the
// block in miniature that carries how real a row is, and the guessed end's
// words. They lived inside the Mac's files until the phone needed the same
// language; one copy is what keeps a likely ghost striped on both.

import SwiftUI

/// The page's own colours, apart from the app palette because they only
/// mean something here.
enum CalendarInk {
    /// The account's hue: the mock's sand. One mailbox is one timeline
    /// today, so one hue; a merged multi-account timeline gives each its
    /// own (docs/CALENDAR.md §4) and this becomes a lookup.
    static let account = Color(light: 0x8C7254, dark: 0xD3C0A6)
}

/// One hue's three tones as SwiftUI colours, each resolving per theme.
struct ChipInks {
    /// A solid chip's body.
    let fill: Color
    /// The hue as text: a row's time, a calendar's name.
    let ink: Color
    /// The hue as a line: stripes, outlines, the agenda's mark.
    let edge: Color
}

/// `CalendarColor`'s tones, built once per hue: a month redraws on every
/// poll tick, and the OKLab round trip is cheap but not free.
@MainActor
enum ChipColors {
    private static var cache: [ChipHue: ChipInks] = [:]

    static func inks(_ hue: ChipHue) -> ChipInks {
        if let hit = cache[hue] { return hit }
        let light = CalendarColor.tones(for: hue, dark: false)
        let dark = CalendarColor.tones(for: hue, dark: true)
        let out = ChipInks(
            fill: Color(light: light.fill.hex, dark: dark.fill.hex),
            ink: Color(light: light.ink.hex, dark: dark.ink.hex),
            edge: Color(light: light.edge.hex, dark: dark.edge.hex))
        // A user has a handful of calendars; a bound keeps a daemon that
        // sends a new colour on every event from growing this forever.
        if cache.count > 256 { cache.removeAll() }
        cache[hue] = out
        return out
    }

    static func inks(_ item: AgendaItem) -> ChipInks { inks(CalendarColor.style(item).hue) }
}

// MARK: - the mark

/// The block in miniature: the one shape that carries how real a row is.
/// A leaf, so the switch on `fill` costs nothing that holds state.
struct AgendaMark: View {
    let fill: AgendaFill
    var estimated = false
    /// Cancelled, or an unlikely row drawn out of its fold: the container
    /// thins further. The words beside it never do.
    var thin = false
    var hue: Color = CalendarInk.account
    var cornerRadius: CGFloat = 3
    /// A flat end where the block goes on past what is drawn: past
    /// midnight, or out of the night cap into the day. Corners tell the
    /// truth about edges (the mock's week notes).
    var flatLeading = false
    var flatTrailing = false
    /// A guessed end's piece after midnight: all guess, so no solid start
    /// of its own. Only with `estimated`.
    var tailOnly = false
    /// A guessed end fades out to nothing instead of stopping at an edge
    /// (the month's strip; the mock's `.strip b.est`). Not where the
    /// block continues past what is drawn.
    var feather = false

    var body: some View {
        let lead: CGFloat = flatLeading ? 0 : cornerRadius
        let trail: CGFloat = flatTrailing ? 0 : cornerRadius
        let shape = UnevenRoundedRectangle(
            topLeadingRadius: lead, bottomLeadingRadius: lead,
            bottomTrailingRadius: trail, topTrailingRadius: trail, style: .continuous)
        ZStack {
            switch thin ? .outline : fill {
            case .solid:
                if estimated {
                    // Known start, guessed body: solid where it begins,
                    // thinning toward where the guess runs out.
                    shape.fill(
                        LinearGradient(
                            stops: [
                                .init(color: hue.opacity(tailOnly ? 0.36 : 0.78), location: tailOnly ? 0 : 0.35),
                                .init(color: hue.opacity(0.22), location: 1),
                            ], startPoint: .leading, endPoint: .trailing)
                    )
                    .mask(
                        LinearGradient(
                            stops: [
                                .init(color: .black, location: feather && !flatTrailing ? 0.6 : 1),
                                .init(color: feather && !flatTrailing ? .clear : .black, location: 1),
                            ], startPoint: .leading, endPoint: .trailing))
                } else {
                    shape.fill(hue.opacity(0.78))
                }
            case .striped:
                Stripes(spacing: 3)
                    .stroke(hue.opacity(0.75), lineWidth: 1.1)
                    .clipShape(shape)
                shape.strokeBorder(hue.opacity(0.8), lineWidth: 1)
            case .outline:
                shape.strokeBorder(hue.opacity(thin ? 0.4 : 0.75), lineWidth: 1)
            }
        }
    }
}

/// Diagonal hatching, top-right to bottom-left: the likely band.
struct Stripes: Shape {
    var spacing: CGFloat

    func path(in rect: CGRect) -> Path {
        var p = Path()
        var x = rect.minX - rect.height
        while x < rect.maxX {
            p.move(to: CGPoint(x: x, y: rect.maxY))
            p.addLine(to: CGPoint(x: x + rect.height, y: rect.minY))
            x += spacing
        }
        return p
    }
}

/// "ends ~11pm": the guessed end in the dim ink with its tilde dimmer
/// still, a stated one a step stronger. The tilde means estimate
/// everywhere, and it is a different colour, not just a character.
struct EndText: View {
    let label: Agenda.EndLabel

    var body: some View {
        if label.estimated, let tilde = label.text.firstIndex(of: "~") {
            let before = String(label.text[..<tilde])
            let after = String(label.text[label.text.index(after: tilde)...])
            Text("\(before)\(Text("~").foregroundStyle(Palette.inkFaintest))\(after)")
                .font(Typo.micro)
                .foregroundStyle(Palette.inkFaint)
        } else {
            Text(label.text)
                .font(Typo.micro)
                .foregroundStyle(Palette.inkDim)
        }
    }
}

/// Mail that did not pass sender authentication made this row. Said, and
/// quietly: a word in the faintest ink with the reason on hover. Not a
/// warning triangle, not the danger or the sealed colour, and the row
/// keeps its full-strength title, because the plan may well be real; it
/// is only that nobody vouched for who wrote it.
struct UnverifiedMark: View {
    var body: some View {
        Text("unverified")
            .font(Typo.micro)
            .foregroundStyle(Palette.inkFaintest)
            .help("The mail this came from did not pass sender authentication, so Passband cannot confirm who sent it.")
    }
}
