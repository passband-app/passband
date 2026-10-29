// THE DISCOVERY RING: seven arcs that fill CLOCKWISE FROM TWELVE as sets are
// unlocked. It says HOW MUCH, never WHICH: with three sets found the first
// three arcs are lit, whichever three they are, so the ring reads as a ring
// filling up rather than as a map with holes in it. It is still a picture, not
// a score: no digits, no fraction, no "next", and nothing ever drains.
//
// THE LIT STRETCH IS ORDERED BY WEIGHT, so the two tiers stay visible:
//
//   * KEPT first, from twelve: the heaviest stroke, its gaps half closed, with
//     a soft phosphor glow. Kept arcs run together, so habits read as one
//     stretch of solid ring.
//   * FOUND next: the passband blue at full weight.
//   * UNSEEN last, the rest of the way round: a hairline in the faintest ink.
//     Present, so the shape is a ring from day one, and quiet enough that it
//     reads as one calm circle rather than as empty slots.
//
// ALL KEPT CLOSES THE LOOP: the gaps go, the seven arcs are one ring, and the
// centre lights. The one state with nothing more to say says so by being whole.
//
// FRESH (found, and the page not visited since) breathes on the NEWEST lit
// arcs: the end of the found stretch, where the ring just grew. A slow 3.2s
// cycle on purpose: a fast pulse is an alarm, and this is a "have a look when
// you like". Reduce Motion holds the glow at its middle value.
//
// The glyph is a pure function of its inputs (a progress, a glow phase), so
// the rail, the page's masthead and the found moment draw the same thing.

import SwiftUI

/// How much of the ring is lit, in the order it is lit. Counts live HERE and
/// only here: they place arcs, and are never shown to anybody.
struct DiscoveryProgress: Equatable, Sendable {
    var kept: Int
    var found: Int
    /// How many of the lit arcs are fresh, counted back from the newest.
    var fresh: Int

    static let arcs = FeatureSet.allCases.count

    init(kept: Int, found: Int, fresh: Int = 0) {
        self.kept = max(0, min(Self.arcs, kept))
        self.found = max(0, min(Self.arcs - self.kept, found))
        self.fresh = max(0, min(self.kept + self.found, fresh))
    }

    init(_ state: (FeatureSet) -> SetState, fresh: Set<FeatureSet>) {
        let states = FeatureSet.allCases.map(state)
        self.init(
            kept: states.count { $0 == .kept },
            found: states.count { $0 == .found },
            // Only a set that is lit can glow: fresh means "found and not yet
            // looked at", never "unseen".
            fresh: fresh.count { state($0) != .unseen })
    }

    var lit: Int { kept + found }
    var allKept: Bool { kept == Self.arcs }

    /// Arc `index`'s state, clockwise from twelve.
    func state(at index: Int) -> SetState {
        index < kept ? .kept : index < lit ? .found : .unseen
    }

    func isFresh(at index: Int) -> Bool { index < lit && index >= lit - fresh }

    /// What VoiceOver says for the ring: how much is found and how much of it
    /// stuck, in words. Spelled out, never digits, and never what is left.
    var spoken: String {
        func words(_ n: Int) -> String {
            let f = NumberFormatter()
            f.numberStyle = .spellOut
            return f.string(from: NSNumber(value: n)) ?? ""
        }
        if lit == 0 { return "Everything here is new to try." }
        if allKept { return "Every way found, and every one a habit." }
        var line = lit == Self.arcs
            ? "Every way found"
            : lit == 1 ? "One way found" : "\(words(lit).capitalized) ways found"
        if kept == lit, kept > 1 { line += ", all of them habits" }
        else if kept == 1 { line += lit == 1 ? ", and it is a habit" : ", one a habit" }
        else if kept > 1 { line += ", \(words(kept)) of them habits" }
        if fresh > 0 { line += fresh == 1 ? ". One is new" : ". Some are new" }
        return line + "."
    }
}

struct DiscoveryRingGlyph: View {
    let progress: DiscoveryProgress
    var diameter: CGFloat = 24
    /// The fresh glow's strength, 0...1. The caller animates it.
    var glow: Double = 0.5

    /// Line weights as fractions of the diameter, so the glyph keeps its
    /// proportions at any size and under window zoom.
    /// Capped above, so the masthead's large emblem stays a drawn ring and
    /// does not swell into a donut.
    private var unseenWeight: CGFloat { min(1.75, max(1, diameter * 0.05)) }
    private var foundWeight: CGFloat { min(4, max(1.8, diameter * 0.11)) }
    private var keptWeight: CGFloat { min(6, max(2.6, diameter * 0.165)) }
    /// Room around the ring for its glow, which a Canvas would otherwise clip
    /// to a visible square.
    private var bleed: CGFloat { max(8, diameter * 0.5) }

    var body: some View {
        Canvas { ctx, size in
            draw(&ctx, size: size)
        }
        .frame(width: diameter + bleed, height: diameter + bleed)
        .accessibilityHidden(true)
    }

    private func draw(_ ctx: inout GraphicsContext, size: CGSize) {
        let center = CGPoint(x: size.width / 2, y: size.height / 2)
        let radius = diameter / 2 - keptWeight / 2
        let slice = 360.0 / Double(DiscoveryProgress.arcs)
        // A gap wide enough to read at 20pt, measured as an ANGLE that keeps a
        // constant visual gap of about 1.6pt whatever the size.
        let baseGap = max(9, Double(1.9 / radius) * 180 / .pi)

        if progress.allKept {
            // The closed loop, with a halo and a lit centre.
            var lit = ctx
            lit.addFilter(.shadow(color: Color(hex: 0x4E9BEA).opacity(0.55), radius: diameter * 0.18))
            let ring = Path(ellipseIn: CGRect(
                x: center.x - radius, y: center.y - radius, width: radius * 2, height: radius * 2))
            lit.stroke(ring, with: .color(DiscoveryInk.ring), lineWidth: keptWeight)
            let dot = diameter * 0.16
            lit.fill(
                Path(ellipseIn: CGRect(x: center.x - dot / 2, y: center.y - dot / 2, width: dot, height: dot)),
                with: .color(DiscoveryInk.ringHot))
            return
        }

        for index in 0..<DiscoveryProgress.arcs {
            let state = progress.state(at: index)
            let weight: CGFloat
            let gap: Double
            switch state {
            case .unseen: (weight, gap) = (unseenWeight, baseGap)
            case .found: (weight, gap) = (foundWeight, baseGap)
            case .kept: (weight, gap) = (keptWeight, baseGap * 0.55)
            }
            // Round caps eat into the gap by half a weight at each end, so the
            // arc is trimmed by that much more to keep the gap honest.
            let capTrim = Double(weight / 2 / radius) * 180 / .pi
            let start = -90 + Double(index) * slice + gap / 2 + capTrim
            let end = -90 + Double(index + 1) * slice - gap / 2 - capTrim
            var arc = Path()
            arc.addArc(
                center: center, radius: radius,
                startAngle: .degrees(start), endAngle: .degrees(max(start + 0.1, end)),
                clockwise: false)
            let style = StrokeStyle(lineWidth: weight, lineCap: .round)

            switch state {
            case .unseen:
                ctx.stroke(arc, with: .color(DiscoveryInk.ringFaint), style: style)
            case .found, .kept:
                let isFresh = progress.isFresh(at: index)
                var layer = ctx
                if state == .kept || isFresh {
                    // The glow is a shadow of the arc itself: even all round,
                    // never one lit edge.
                    let strength = isFresh ? 0.35 + 0.55 * glow : 0.4
                    layer.addFilter(.shadow(
                        color: Color(hex: 0x4E9BEA).opacity(strength),
                        radius: diameter * (isFresh ? 0.12 + 0.1 * glow : 0.1)))
                }
                layer.stroke(
                    arc, with: .color(isFresh ? DiscoveryInk.ringHot : DiscoveryInk.ring), style: style)
            }
        }
    }
}

/// The ring's and the page's theme-aware inks.
enum DiscoveryInk {
    /// A found arc: the accent.
    static let ring = Palette.accent
    /// A fresh arc and the all-kept centre: a step brighter than the accent
    /// in dark, a step deeper in light, so it reads as "lit" on either ground.
    static let ringHot = Color(light: 0x1C6FD0, dark: 0x8CC4FF)
    /// An unseen arc. Ink, not blue: blue is for what is found.
    static let ringFaint = Color(
        light: Color(hex: 0x1E3246).opacity(0.22), dark: Color(hex: 0xFFFFFF).opacity(0.2))
}

/// The breathing value for the fresh glow, as a pure function of time so the
/// rail can drive it from a paused-when-idle `TimelineView`.
enum DiscoveryBreath {
    static let period: Double = 3.2

    static func glow(at date: Date) -> Double {
        let phase = date.timeIntervalSinceReferenceDate.truncatingRemainder(dividingBy: period) / period
        return 0.5 - 0.5 * cos(phase * 2 * .pi)
    }
}

/// The ring as it sits on the rail and in the page's masthead: the glyph plus
/// the breathing clock, which runs only while something is fresh.
struct DiscoveryRing: View {
    let progress: DiscoveryProgress
    var diameter: CGFloat = 24
    /// False holds the breath still (window not key, Reduce Motion).
    var breathing = true

    var body: some View {
        // Paused, the timeline hands back a single date and draws once: a
        // ring with nothing fresh costs exactly one Canvas pass.
        TimelineView(.animation(minimumInterval: 1 / 20, paused: progress.fresh == 0 || !breathing)) { timeline in
            let glow = breathing ? DiscoveryBreath.glow(at: timeline.date) : 0.5
            DiscoveryRingGlyph(progress: progress, diameter: diameter, glow: glow)
                // FRESH LIGHTS THE TILE, not just the arc: a soft wash of the
                // accent inside the ring, breathing with it. An arc's glow
                // alone vanishes on a light ground; a filled disc reads on
                // both, and it is a colour change, not a badge.
                .background {
                    Circle()
                        .fill(Palette.accent.opacity(progress.fresh == 0 ? 0 : 0.10 + 0.14 * glow))
                        .frame(width: diameter * 0.78, height: diameter * 0.78)
                        .blur(radius: diameter * 0.06)
                }
        }
    }
}
