import SwiftUI

/// NOT NOW: `h` lifts the email off the board and its place is kept as a
/// dashed outline. A point of light carries it along a day's arc, the arc
/// drawing on behind it: time passing, with no clock and no numbers. At the
/// arc's end the email drops back into its place by itself, lit.
enum LaterVignette: VignetteScene {
    private static let press: Double = 0.7

    /// Away, mid-arc: the empty place, the light halfway across the day.
    static let posterTime: Double = 2.0

    private static let card = CGRect(x: 16, y: 40, width: 98, height: 30)

    // The arc: a shallow circle segment over the card, (24, 31) to (106, 31),
    // peaking at y 13.
    private static let center = CGPoint(x: 65, y: 13 + 55.7)
    private static let radius: CGFloat = 55.7
    private static let a0 = atan2(31 - (13 + 55.7), 24 - 65.0)
    private static let a1 = atan2(31 - (13 + 55.7), 106 - 65.0)

    private static func point(_ p: Double) -> CGPoint {
        let a = a0 + (a1 - a0) * p
        return CGPoint(x: center.x + radius * CGFloat(cos(a)), y: center.y + radius * CGFloat(sin(a)))
    }

    private static func arc(to p: Double) -> Path {
        var path = Path()
        let steps = 40
        path.move(to: point(0))
        for i in 1...steps { path.addLine(to: point(p * Double(i) / Double(steps))) }
        return path
    }

    static func draw(_ ctx: inout GraphicsContext, t: Double, set: FeatureSet) {
        // The day, always faintly there: an arc with a mark at each end.
        ctx.stroke(
            arc(to: 1), with: .color(VignetteInk.dim.opacity(0.7)),
            style: StrokeStyle(lineWidth: 1, lineCap: .round, dash: [1, 4]))
        for p in [0.0, 1.0] {
            let q = point(p)
            ctx.fill(Path(ellipseIn: CGRect(x: q.x - 1.5, y: q.y - 1.5, width: 3, height: 3)), with: .color(VignetteInk.dim))
        }

        let leave = VignetteMotion.ease(t, press + 0.12, press + 0.5)
        let travel = VignetteMotion.ease(t, 1.24, 2.84)
        let dot = VignetteMotion.ease(t, 1.08, 1.26) * (1 - VignetteMotion.ease(t, 2.84, 3.0))
        let back = VignetteMotion.ease(t, 2.94, 3.36)
        let gone = leave * (1 - back)

        // The kept place.
        if gone > 0.001 {
            ctx.stroke(
                Path(roundedRect: card, cornerRadius: 7, style: .continuous),
                with: .color(VignetteInk.dim.opacity(0.9 * gone)),
                style: StrokeStyle(lineWidth: 0.75, dash: [3, 3]))
        }

        // The trace of the day, drawn by the light and let go once it is back.
        let trace = 1 - VignetteMotion.ease(t, 3.4, 3.9)
        if travel > 0, trace > 0 {
            var layer = ctx
            layer.opacity = trace
            layer.addFilter(.shadow(color: VignetteInk.phosphor.opacity(0.7), radius: 3))
            layer.stroke(
                arc(to: travel), with: .color(VignetteInk.phosphor),
                style: StrokeStyle(lineWidth: 1.5, lineCap: .round))
        }

        // The email, as a point of light on its way.
        if dot > 0.001 {
            let q = point(travel)
            var layer = ctx
            layer.opacity = dot
            layer.addFilter(.shadow(color: VignetteInk.phosphor, radius: 5))
            layer.fill(Path(ellipseIn: CGRect(x: q.x - 3.5, y: q.y - 3.5, width: 7, height: 7)), with: .color(VignetteInk.phosphorHot))
        }

        // The card: up and out toward the arc's start, back down into place.
        let y = card.minY - 10 * CGFloat(gone)
        VignetteParts.mail(&ctx, origin: CGPoint(x: card.minX, y: y), design: 0, opacity: 1 - gone)
        let lit = VignetteMotion.ease(t, 3.2, 3.4) * (1 - VignetteMotion.ease(t, 3.6, 4.15))
        VignetteParts.halo(&ctx, rect: card, cornerRadius: 7, amount: lit)

        VignetteParts.keycap(&ctx, "h", origin: CGPoint(x: 130, y: 44), pressed: VignetteMotion.press(t, at: press))
    }
}
