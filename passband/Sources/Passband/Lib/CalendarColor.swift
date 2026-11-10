// THE CALENDAR'S COLOURS: what a Google calendar's hex becomes inside
// Passband's palette, and which colour and fill every calendar row is drawn in.
//
// PURE, and Foundation only, so test.sh can hold it against fixtures
// (Tests/MonthGridTests.swift). The views turn the three tones below into
// SwiftUI colours, once per hex (CalendarMonth.swift, `ChipColors`).
//
// THE PROBLEM. Google hands over arbitrary sRGB, and its own palette is loud:
// Tomato, Banana, Peacock and Grape at full saturation, picked by the user
// one calendar at a time with no thought for how they sit together. Drawn as
// given, a month of them is confetti on a pale blue glass app. Muted to grey,
// the colour that tells calendars apart is lost.
//
// THE ANSWER: KEEP THE HUE, OWN THE LIGHTNESS AND THE CHROMA. Each hex is read
// into OKLCH (a perceptual space: equal steps of L look equally light whatever
// the hue), its hue kept exactly, and then placed on Passband's own lightness
// for the theme with its chroma capped. Every calendar's chips are then the
// SAME lightness and at most the same saturation: they differ only in hue, so
// a month reads as one set of inks rather than a pile of stickers, and Banana
// stops shouting over Graphite. A calendar the user made grey stays grey,
// because the cap is a ceiling and never a floor.
//
// Three tones per hue and theme:
//   - fill: the chip's body. Light enough under ink in light mode, a deep
//     well in dark mode, both clear of the canvas they sit on.
//   - ink: the hue as text, for the time on a chip and the calendar's name.
//     At least 4.5:1 against its own fill in both themes (tested).
//   - edge: the hue as a line, for the striped and outline fills, which are
//     the likelihood scale and must stay readable on any hue.

import Foundation

/// An sRGB colour, 0...1 a channel.
struct RGB: Hashable, Sendable {
    var r: Double
    var g: Double
    var b: Double

    init(r: Double, g: Double, b: Double) {
        (self.r, self.g, self.b) = (r, g, b)
    }

    init(hex: UInt32) {
        self.init(
            r: Double((hex >> 16) & 0xFF) / 255, g: Double((hex >> 8) & 0xFF) / 255,
            b: Double(hex & 0xFF) / 255)
    }

    /// The nearest 24-bit value.
    var hex: UInt32 {
        func byte(_ v: Double) -> UInt32 { UInt32((min(1, max(0, v)) * 255).rounded()) }
        return byte(r) << 16 | byte(g) << 8 | byte(b)
    }

    /// WCAG relative luminance, for the contrast the tests hold the tones to.
    var luminance: Double {
        func lin(_ c: Double) -> Double { c <= 0.04045 ? c / 12.92 : pow((c + 0.055) / 1.055, 2.4) }
        return 0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b)
    }

    static func contrast(_ a: RGB, _ b: RGB) -> Double {
        let (x, y) = (a.luminance, b.luminance)
        return (max(x, y) + 0.05) / (min(x, y) + 0.05)
    }
}

/// A colour in OKLCH: lightness 0...1, chroma, hue in degrees.
struct OKLCH: Hashable, Sendable {
    var l: Double
    var c: Double
    var h: Double
}

/// Which colour a row is drawn in.
enum ChipHue: Hashable, Sendable {
    /// A real event: its calendar's colour (or the event's own override), as
    /// the daemon served it.
    case calendar(RGB)
    /// Anything Passband found in mail: one hue of its own, the accent's.
    case ghost
    /// A real event with no colour, from a daemon too old to send one: the
    /// warm neutral the page drew everything in before calendars had colours.
    case plain
}

/// How one row is drawn: its hue, its fill on the likelihood scale, and the
/// two marks that say it is off.
struct ChipStyle: Hashable, Sendable {
    let hue: ChipHue
    let fill: AgendaFill
    /// The container thins further: cancelled, or an unlikely ghost let out
    /// of its fold. Never the words.
    let thin: Bool
    /// The title is struck through.
    let struck: Bool
}

enum CalendarColor {
    // MARK: the palette's own anchors

    /// The ghost hue: Passband's accent (`Palette.accent`, light). Ghosts are
    /// Passband's, so they wear its family; the band language inside it says
    /// how real each one is.
    static let ghostBase = RGB(hex: 0x2B7FD4)
    /// The warm neutral for an event that came with no colour (the mock's
    /// sand, `CalendarInk.account`'s light value).
    static let plainBase = RGB(hex: 0x8C7254)

    /// Where each tone sits, per theme: a lightness, and a chroma ceiling.
    /// Light: the fill sits clearly under the pale blue canvas (L ~0.94) so a
    /// chip reads as a chip; dark: a well a step above the blue-black canvas
    /// (L ~0.17). The ink is the hue at text strength; the edge between.
    struct Target: Hashable, Sendable {
        let l: Double
        let maxChroma: Double
    }

    static let lightFill = Target(l: 0.885, maxChroma: 0.062)
    static let lightInk = Target(l: 0.43, maxChroma: 0.11)
    static let lightEdge = Target(l: 0.60, maxChroma: 0.14)
    static let darkFill = Target(l: 0.355, maxChroma: 0.062)
    static let darkInk = Target(l: 0.86, maxChroma: 0.085)
    static let darkEdge = Target(l: 0.70, maxChroma: 0.12)

    // MARK: parsing

    /// `#rrggbb` (Google's form), `rrggbb`, or the short `#rgb`. Anything else
    /// is no colour, and the row falls back to `plain`.
    static func parse(_ raw: String?) -> RGB? {
        guard var s = raw?.trimmingCharacters(in: .whitespacesAndNewlines), !s.isEmpty else { return nil }
        if s.hasPrefix("#") { s.removeFirst() }
        if s.count == 3 { s = s.map { "\($0)\($0)" }.joined() }
        guard s.count == 6, s.allSatisfy(\.isHexDigit), let v = UInt32(s, radix: 16) else { return nil }
        return RGB(hex: v)
    }

    // MARK: resolution

    /// Which hue and fill a row is drawn in. A real event takes its calendar's
    /// colour and is solid; a ghost takes the ghost hue and keeps its band
    /// (solid committed, striped likely, outline maybe); cancelled is struck
    /// and thinned; an unlikely ghost drawn out of its fold is thinned.
    static func style(_ item: AgendaItem) -> ChipStyle {
        if let event = item.event {
            let hue: ChipHue = parse(event.color).map(ChipHue.calendar) ?? .plain
            return ChipStyle(hue: hue, fill: item.fill, thin: item.cancelled, struck: item.cancelled)
        }
        return ChipStyle(
            hue: .ghost, fill: item.fill, thin: item.cancelled || item.folded, struck: item.cancelled)
    }

    static func base(_ hue: ChipHue) -> RGB {
        switch hue {
        case .calendar(let rgb): rgb
        case .ghost: ghostBase
        case .plain: plainBase
        }
    }

    // MARK: the three tones

    struct Tones: Hashable, Sendable {
        let fill: RGB
        let ink: RGB
        let edge: RGB
    }

    /// A hue's three tones for one theme.
    static func tones(_ base: RGB, dark: Bool, fillChroma: Double = 1) -> Tones {
        let lch = oklch(base)
        func place(_ t: Target, share: Double = 1) -> RGB {
            fitted(OKLCH(l: t.l, c: min(lch.c, t.maxChroma * share), h: lch.h))
        }
        return dark
            ? Tones(fill: place(darkFill, share: fillChroma), ink: place(darkInk), edge: place(darkEdge))
            : Tones(fill: place(lightFill, share: fillChroma), ink: place(lightInk), edge: place(lightEdge))
    }

    /// The share of the fill's chroma a ghost gets. Google's commonest
    /// calendar colours are blues (Peacock is many accounts' default), and a
    /// ghost at full chroma in the accent's hue was a near twin of them: a
    /// committed ghost and a real Peacock event were one chip but for the
    /// envelope. At this share the ghost's body is a quieter, greyer blue
    /// than any calendar's, while its EDGE (the stripes and the outline that
    /// carry the band) keeps full strength.
    static let ghostFillChroma = 0.55

    /// The tones a row is drawn in, for its hue.
    static func tones(for hue: ChipHue, dark: Bool) -> Tones {
        tones(base(hue), dark: dark, fillChroma: hue == .ghost ? ghostFillChroma : 1)
    }

    // MARK: OKLab

    static func oklch(_ c: RGB) -> OKLCH {
        func lin(_ v: Double) -> Double { v <= 0.04045 ? v / 12.92 : pow((v + 0.055) / 1.055, 2.4) }
        let (r, g, b) = (lin(c.r), lin(c.g), lin(c.b))
        let l = cbrt(0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b)
        let m = cbrt(0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b)
        let s = cbrt(0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b)
        let L = 0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s
        let A = 1.9779984951 * l - 2.4285922050 * m + 0.4505937099 * s
        let B = 0.0259040371 * l + 0.7827717662 * m - 0.8086757660 * s
        var h = atan2(B, A) * 180 / .pi
        if h < 0 { h += 360 }
        return OKLCH(l: L, c: (A * A + B * B).squareRoot(), h: h)
    }

    /// Unclamped: channels can fall outside 0...1 for a colour sRGB cannot show.
    static func rgbUnclamped(_ c: OKLCH) -> RGB {
        let A = c.c * cos(c.h * .pi / 180)
        let B = c.c * sin(c.h * .pi / 180)
        let l = pow(c.l + 0.3963377774 * A + 0.2158037573 * B, 3)
        let m = pow(c.l - 0.1055613458 * A - 0.0638541728 * B, 3)
        let s = pow(c.l - 0.0894841775 * A - 1.2914855480 * B, 3)
        func gamma(_ v: Double) -> Double { v <= 0.0031308 ? 12.92 * v : 1.055 * pow(v, 1 / 2.4) - 0.055 }
        return RGB(
            r: gamma(4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s),
            g: gamma(-1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s),
            b: gamma(-0.0041960863 * l - 0.7034186147 * m + 1.7076147010 * s))
    }

    /// Into sRGB by giving up chroma, never lightness or hue: a deep yellow
    /// ink does not exist, so it becomes a less saturated one at the same
    /// lightness rather than a lighter, louder one.
    static func fitted(_ c: OKLCH) -> RGB {
        func inGamut(_ x: RGB) -> Bool {
            [x.r, x.g, x.b].allSatisfy { $0 >= -0.0005 && $0 <= 1.0005 }
        }
        var probe = c
        var out = rgbUnclamped(probe)
        var tries = 0
        while !inGamut(out), tries < 30 {
            probe.c *= 0.9
            out = rgbUnclamped(probe)
            tries += 1
        }
        return RGB(r: min(1, max(0, out.r)), g: min(1, max(0, out.g)), b: min(1, max(0, out.b)))
    }
}
