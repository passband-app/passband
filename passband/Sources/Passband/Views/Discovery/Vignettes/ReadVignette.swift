import SwiftUI

/// YOUR READING: newsletter issues, each with its masthead block, gather into
/// a stack; a new one lands on top. Then `hold e`: the key stays down while
/// a ring draws itself closed, and when it closes the whole stack lifts away
/// at once. A hold, not a tap. Then two issues gather again.
enum ReadVignette: VignetteScene {
    private static let holdFrom: Double = 1.4
    /// The app's ring takes 750ms to close.
    private static let ringFrom: Double = 1.46
    private static let ringTo: Double = 2.21
    private static let holdTo: Double = 2.26

    /// Held: the stack of three, the ring two thirds closed, the key down.
    static let posterTime: Double = 1.96

    private static let deck = CGPoint(x: 16, y: 26)
    private static let width: CGFloat = 92
    private static let poseY: [CGFloat] = [0, 7, 13, 18]
    private static let poseScale: [CGFloat] = [1, 0.93, 0.86, 0.8]
    private static let poseOpacity: [Double] = [1, 0.7, 0.4, 0]

    private static let ring = CGPoint(x: 138, y: 31)
    private static let ringRadius: CGFloat = 8

    static func draw(_ ctx: inout GraphicsContext, t: Double, set: FeatureSet) {
        // The lane header: Reading.
        VignetteParts.bar(&ctx, CGRect(x: deck.x, y: 13, width: 24, height: 3), color: VignetteInk.phosphor, opacity: 0.7)
        VignetteParts.bar(&ctx, CGRect(x: deck.x + 28, y: 13, width: 12, height: 3), color: VignetteInk.dim, opacity: 1)

        // A new issue lands on top; the others step back.
        let arrive = VignetteMotion.ease(t, 0.5, 0.95)
        // The stack clears together once the ring closes...
        let clear = VignetteMotion.ease(t, ringTo + 0.04, ringTo + 0.4)
        // ...and two issues gather again before the seam.
        let regather = VignetteMotion.ease(t, 3.7, 4.1)

        if t < 3.0 {
            // Before the hold: designs [1 front, 2 behind], then 0 lands on top.
            let stack: [(design: Int, depth: Double)] = [(2, 1 + arrive), (1, arrive)]
            let lift = -16 * CGFloat(clear)
            for card in stack {
                pose(&ctx, design: card.design, depth: card.depth, dy: lift, fade: 1 - clear)
            }
            if arrive > 0 {
                // In from the right, solid early so it never shows through.
                let shown = VignetteMotion.ease(t, 0.5, 0.66)
                card(
                    &ctx, origin: CGPoint(x: deck.x + 22 * CGFloat(1 - arrive), y: deck.y + lift),
                    design: 0, opacity: shown * (1 - clear), scale: 1)
            }
        } else if regather > 0 {
            pose(&ctx, design: 2, depth: 1, dy: 0, fade: regather)
            pose(&ctx, design: 1, depth: 0, dy: -6 * CGFloat(1 - regather), fade: regather)
        }

        // The ring: a faint track while held, drawn closed clockwise from the
        // top, bright for a moment when it closes, then let go.
        let shown = VignetteMotion.ease(t, holdFrom, holdFrom + 0.08) * (1 - VignetteMotion.ease(t, ringTo + 0.3, ringTo + 0.6))
        if shown > 0.001 {
            let progress = min(1, max(0, (t - ringFrom) / (ringTo - ringFrom)))
            let circle = Path(ellipseIn: CGRect(
                x: ring.x - ringRadius, y: ring.y - ringRadius, width: 2 * ringRadius, height: 2 * ringRadius))
            var track = ctx
            track.opacity = shown
            track.stroke(circle, with: .color(VignetteInk.dim), lineWidth: 1.5)
            var arc = Path()
            arc.addArc(
                center: ring, radius: ringRadius, startAngle: .degrees(-90),
                endAngle: .degrees(-90 + 360 * progress), clockwise: false)
            let closed = VignetteMotion.ease(t, ringTo - 0.02, ringTo + 0.08)
            var lit = ctx
            lit.opacity = shown
            lit.addFilter(.shadow(color: VignetteInk.phosphor.opacity(0.6 + 0.4 * closed), radius: 3 + 3 * closed))
            lit.stroke(
                arc, with: .color(closed > 0.5 ? VignetteInk.phosphorHot : VignetteInk.phosphor),
                style: StrokeStyle(lineWidth: 1.5, lineCap: .round))
        }

        VignetteParts.keycap(&ctx, "hold e", origin: CGPoint(x: 116, y: 52), width: 44, pressed: held(t))
    }

    /// A key held down: the usual press in, kept down for the whole hold.
    private static func held(_ t: Double) -> Double {
        if t < holdFrom || t > holdTo + 0.08 { return 0 }
        if t < holdFrom + 0.06 { return (t - holdFrom) / 0.06 }
        if t <= holdTo { return 1 }
        return 1 - (t - holdTo) / 0.08
    }

    /// An issue at a (fractional) depth in the stack.
    private static func pose(_ ctx: inout GraphicsContext, design: Int, depth: Double, dy: CGFloat, fade: Double) {
        let lo = min(2, Int(depth.rounded(.down))), hi = min(3, lo + 1)
        let f = depth - Double(lo)
        card(
            &ctx,
            origin: CGPoint(x: deck.x, y: deck.y + VignetteMotion.mix(poseY[lo], poseY[hi], f) + dy),
            design: design,
            opacity: (poseOpacity[lo] + (poseOpacity[hi] - poseOpacity[lo]) * f) * fade,
            scale: VignetteMotion.mix(poseScale[lo], poseScale[hi], f))
    }

    /// One issue: a mail card's ground, but a masthead block where the
    /// avatar would be and a few lines of reading beside it.
    private static func card(_ ctx: inout GraphicsContext, origin: CGPoint, design: Int, opacity: Double, scale s: CGFloat) {
        guard opacity > 0.001 else { return }
        let designs: [(title: CGFloat, a: CGFloat, b: CGFloat)] = [(38, 50, 36), (30, 44, 50), (44, 38, 30)]
        let d = designs[design % 3]
        let w = width * s, h = 30 * s
        let rect = CGRect(x: origin.x + (width - w) / 2, y: origin.y, width: w, height: h)
        VignetteParts.cardGround(&ctx, rect: rect, cornerRadius: 7 * s, opacity: opacity)
        var layer = ctx
        layer.opacity = opacity
        let mast = CGRect(x: rect.minX + 6 * s, y: rect.minY + 6 * s, width: 15 * s, height: 18 * s)
        layer.fill(
            Path(roundedRect: mast, cornerRadius: 2.5 * s, style: .continuous),
            with: .color(VignetteInk.avatar[design % 3].opacity(0.55)))
        layer.fill(
            Path(roundedRect: CGRect(x: mast.minX, y: mast.minY, width: mast.width, height: 4 * s), cornerRadius: 1.5 * s),
            with: .color(VignetteInk.avatar[design % 3].opacity(0.9)))
        let x = mast.maxX + 6 * s
        layer.fill(
            Path(roundedRect: CGRect(x: x, y: rect.minY + 7 * s, width: d.title * s, height: 3.5 * s), cornerRadius: 1.75 * s),
            with: .color(VignetteInk.ink.opacity(0.8)))
        for (i, bw) in [d.a, d.b].enumerated() {
            layer.fill(
                Path(roundedRect: CGRect(x: x, y: rect.minY + (14 + 6 * CGFloat(i)) * s, width: bw * s, height: 2.6 * s), cornerRadius: 1.3 * s),
                with: .color(VignetteInk.dim))
        }
    }
}
