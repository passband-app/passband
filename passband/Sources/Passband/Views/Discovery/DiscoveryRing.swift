// THE DISCOVERY RING — eight arcs, one per feature set, in `FeatureSet` order
// clockwise from twelve. It is a picture of what somebody has found, never a
// score: no digits, no fraction, no "next", and nothing ever drains.
//
// THREE STATES BY WEIGHT, NOT JUST BY COLOUR, so they survive a 20pt glyph and
// a colour-blind eye alike:
//
//   * UNSEEN is a hairline in the faintest ink. Present, so the shape is a
//     ring from day one, and quiet enough that eight of them read as one calm
//     circle rather than eight empty slots.
//   * FOUND is the passband blue at full weight.
//   * KEPT is heavier still, its gaps half closed, with a soft phosphor glow.
//     Kept neighbours run together, so a habit reads as a stretch of solid
//     ring rather than as another tick.
//
// ALL KEPT CLOSES THE LOOP: the gaps go, the eight arcs are one ring, and the
// centre lights. The one state with nothing more to say says so by being whole.
//
// FRESH (found, and the menu not opened since) breathes: the fresh arcs carry a
// glow that rises and falls on a slow 3.2s cycle. Slow on purpose: a fast pulse
// is an alarm, and this is a "have a look when you like". Reduce Motion holds
// the glow at its middle value.
//
// The glyph is a pure function of its inputs (states, fresh, a glow phase), so
// the rail button, the menu's emblem and the found moment draw the same thing
// at three sizes.

import SwiftUI

struct DiscoveryRingGlyph: View {
    /// One state per set, in `FeatureSet.allCases` order.
    let states: [SetState]
    var fresh: Set<FeatureSet> = []
    /// Only this set's arc is drawn in its state; the rest stay hairlines. The
    /// menu tiles use it to say "this is the slice that tile is".
    var only: FeatureSet? = nil
    var diameter: CGFloat = 24
    /// The fresh glow's strength, 0...1. The caller animates it.
    var glow: Double = 0.5
    /// One ink for the whole glyph, for a ring drawn ON the accent (the kept
    /// tag), where blue arcs would vanish.
    var ink: Color? = nil

    /// Line weights as fractions of the diameter, so the glyph keeps its
    /// proportions at any size and under window zoom.
    /// Capped above, so the menu's large emblem stays a drawn ring and does
    /// not swell into a donut.
    private var unseenWeight: CGFloat { min(1.75, max(1, diameter * 0.05)) }
    private var foundWeight: CGFloat { min(4, max(1.8, diameter * 0.11)) }
    private var keptWeight: CGFloat { min(6, max(2.6, diameter * 0.165)) }
    /// Room around the ring for its glow, which a Canvas would otherwise clip
    /// to a visible square.
    private var bleed: CGFloat { max(8, diameter * 0.5) }

    private var allKept: Bool { only == nil && states.allSatisfy { $0 == .kept } }

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
        let slice = 360.0 / Double(FeatureSet.allCases.count)
        // A gap wide enough to read at 20pt, measured as an ANGLE that keeps a
        // constant visual gap of about 1.6pt whatever the size.
        let baseGap = max(9, Double(1.9 / radius) * 180 / .pi)

        let accent = ink ?? DiscoveryInk.ring
        let hot = ink ?? DiscoveryInk.ringHot
        let faint = ink?.opacity(0.4) ?? DiscoveryInk.ringFaint

        if allKept {
            // The closed loop, with a halo and a lit centre.
            var halo = ctx
            halo.addFilter(.shadow(color: Color(hex: 0x4E9BEA).opacity(0.55), radius: diameter * 0.18))
            let ring = Path(ellipseIn: CGRect(
                x: center.x - radius, y: center.y - radius, width: radius * 2, height: radius * 2))
            halo.stroke(ring, with: .color(accent), lineWidth: keptWeight)
            let dot = diameter * 0.16
            halo.fill(
                Path(ellipseIn: CGRect(x: center.x - dot / 2, y: center.y - dot / 2, width: dot, height: dot)),
                with: .color(hot))
            return
        }

        for (index, set) in FeatureSet.allCases.enumerated() {
            let state = only == nil || only == set ? states[index] : .unseen
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
                ctx.stroke(arc, with: .color(faint), style: style)
            case .found, .kept:
                let isFresh = fresh.contains(set)
                var layer = ctx
                if state == .kept || isFresh {
                    // The glow is a shadow of the arc itself: even all round,
                    // never one lit edge.
                    let strength = isFresh ? 0.35 + 0.55 * glow : 0.4
                    layer.addFilter(.shadow(
                        color: Color(hex: 0x4E9BEA).opacity(strength),
                        radius: diameter * (isFresh ? 0.12 + 0.1 * glow : 0.1)))
                }
                layer.stroke(arc, with: .color(isFresh ? hot : accent), style: style)
            }
        }
    }
}

/// The ring's and the vignettes' theme-aware inks.
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

/// The ring as it sits on the rail and in the menu's header: the glyph plus
/// the breathing clock, which runs only while something is fresh.
struct DiscoveryRing: View {
    let states: [SetState]
    let fresh: Set<FeatureSet>
    var diameter: CGFloat = 24
    /// False holds the breath still (window not key, Reduce Motion).
    var breathing = true

    var body: some View {
        // Paused, the timeline hands back a single date and draws once: a
        // ring with nothing fresh costs exactly one Canvas pass.
        TimelineView(.animation(minimumInterval: 1 / 20, paused: fresh.isEmpty || !breathing)) { timeline in
            let glow = breathing ? DiscoveryBreath.glow(at: timeline.date) : 0.5
            DiscoveryRingGlyph(states: states, fresh: fresh, diameter: diameter, glow: glow)
                // FRESH LIGHTS THE TILE, not just the arc: a soft wash of the
                // accent inside the ring, breathing with it. An arc's glow
                // alone vanishes on a light ground; a filled disc reads on
                // both, and it is a colour change, not a badge.
                .background {
                    Circle()
                        .fill(Palette.accent.opacity(fresh.isEmpty ? 0 : 0.10 + 0.14 * glow))
                        .frame(width: diameter * 0.78, height: diameter * 0.78)
                        .blur(radius: diameter * 0.06)
                }
        }
    }
}

extension FeatureSet {
    /// Every set's state, in ring order, from a lookup.
    static func ringStates(_ state: (FeatureSet) -> SetState) -> [SetState] {
        allCases.map(state)
    }

    /// What VoiceOver says for a whole ring: each set by name and state, the
    /// found and kept first, because that is the good news.
    static func ringDescription(_ state: (FeatureSet) -> SetState) -> String {
        let ordered = allCases.sorted { state($0) > state($1) }
        return ordered.map { "\($0.title), \($0.stateWords(state($0)))" }.joined(separator: "; ")
    }
}
