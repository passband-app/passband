import SwiftUI

/// TEACH PASSBAND: `t`, and a line is written once, about one sender (the
/// violet dot). The line settles into a lit rule. Then mail arrives: two from
/// that sender slide quietly aside into a small pile on their own, and the
/// one that matters (blue) lands and stays. Then the picture rests and
/// clears, back to the empty line.
enum TeachVignette: VignetteScene {
    private static let press: Double = 0.5

    /// After: the rule lit, the pile of two set aside, the one that matters
    /// in place. Cause and lasting effect in one frame.
    static let posterTime: Double = 3.9

    private static let strip = CGRect(x: 16, y: 11, width: 100, height: 18)
    private static let slot = CGPoint(x: 16, y: 41)
    /// The sender the rule is about.
    private static let taught = 1

    /// Arrivals: when each lands, and when (if) it is routed aside.
    private static let arrivals: [(at: Double, design: Int, aside: Double?)] = [
        (1.5, taught, 1.95), (2.4, taught, 2.85), (3.3, 0, nil),
    ]

    static func draw(_ ctx: inout GraphicsContext, t: Double, set: FeatureSet) {
        // Everything this loop made fades together, back to the empty line.
        // The keycap and the empty strip are outside it and never move.
        // (A context's `opacity` is SET, not multiplied, by the parts, so the
        // fade is carried as a factor rather than as a faded context.)
        let reset = VignetteMotion.ease(t, 4.1, 4.42)
        let keep = 1 - reset

        // The line you write: a pill with the sender's dot.
        let lock = VignetteMotion.ease(t, 1.18, 1.42) * (1 - reset)
        let pill = Path(roundedRect: strip, cornerRadius: strip.height / 2, style: .continuous)
        ctx.fill(pill, with: .color(VignetteInk.faint))
        ctx.stroke(pill, with: .color(VignetteInk.edge), lineWidth: 0.6)
        if lock > 0.001 {
            var lit = ctx
            lit.opacity = lock
            lit.addFilter(.shadow(color: VignetteInk.phosphor.opacity(0.6), radius: 4))
            lit.stroke(pill, with: .color(VignetteInk.phosphor.opacity(0.8)), lineWidth: 0.9)
        }
        ctx.fill(
            Path(ellipseIn: CGRect(x: strip.minX + 6, y: strip.midY - 4, width: 8, height: 8)),
            with: .color(VignetteInk.avatar[taught].opacity(0.85)))

        let write = VignetteMotion.ease(t, press + 0.1, 1.2)
        let barX = strip.minX + 20
        if write > 0 {
            var bar = ctx
            bar.opacity = keep
            bar.fill(
                Path(roundedRect: CGRect(x: barX, y: strip.midY - 1.75, width: 64 * write, height: 3.5), cornerRadius: 1.75),
                with: .color(VignetteInk.ink.opacity(0.8)))
        }
        // The caret while writing.
        let caret = VignetteMotion.ease(t, press + 0.04, press + 0.1) * (1 - VignetteMotion.ease(t, 1.2, 1.32))
        if caret > 0.001 {
            var c = ctx
            c.opacity = caret * keep
            c.fill(
                Path(roundedRect: CGRect(x: barX + 64 * write + 2, y: strip.midY - 4.5, width: 1.5, height: 9), cornerRadius: 0.75),
                with: .color(VignetteInk.phosphorHot))
        }

        // The pile where routed mail rests, quietly.
        let pile = [CGRect(x: 124, y: 62, width: 32, height: 9), CGRect(x: 124, y: 51, width: 32, height: 9)]

        var landed = 0
        for a in arrivals {
            let arrive = VignetteMotion.ease(t, a.at, a.at + 0.3)
            guard arrive > 0 else { continue }
            let go = a.aside.map { VignetteMotion.ease(t, $0, $0 + 0.36) } ?? 0
            if go < 1 {
                VignetteParts.mail(
                    &ctx, origin: CGPoint(x: slot.x + 30 * CGFloat(go), y: slot.y - 6 * CGFloat(1 - arrive)),
                    design: a.design, opacity: arrive * (1 - go) * keep)
            }
            if let aside = a.aside, landed < pile.count {
                let rest = VignetteMotion.ease(t, aside + 0.16, aside + 0.42)
                VignetteParts.miniMail(&ctx, rect: pile[landed], design: a.design, opacity: 0.6 * rest * keep)
                landed += 1
            }
        }

        VignetteParts.keycap(&ctx, "t", origin: CGPoint(x: 130, y: 9), pressed: VignetteMotion.press(t, at: press))
    }
}
