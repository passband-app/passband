import SwiftUI

/// SMART CLEAR: Reading's list of four, two of them already faded (the ones
/// that have run their course). The pill in the corner is pressed and stays
/// lit while it works; the two faded rows slide away together, the rest close
/// the gap, and a receipt with a check says it is done. Not a hold and not a
/// key (that is `read`): one press on a pill, and only the stale mail goes.
enum SweepVignette: VignetteScene {
    private static let press: Double = 0.7

    /// Pressed and working: the pill lit, the stale rows halfway gone.
    static let posterTime: Double = 1.22

    private static let x: CGFloat = 16
    private static let width: CGFloat = 88
    private static let height: CGFloat = 10
    private static let slots: [CGFloat] = [24, 36, 48, 60]
    /// Rows 1 and 3 have run their course.
    private static let stale: Set<Int> = [1, 3]

    private static let pill = CGRect(x: 112, y: 9, width: 44, height: 13)
    private static let receipt = CGRect(x: 116, y: 34, width: 40, height: 30)

    static func draw(_ ctx: inout GraphicsContext, t: Double, set: FeatureSet) {
        // The lane header: Reading.
        VignetteParts.bar(&ctx, CGRect(x: x, y: 13, width: 24, height: 3), color: VignetteInk.phosphor, opacity: 0.7)
        VignetteParts.bar(&ctx, CGRect(x: x + 28, y: 13, width: 12, height: 3), color: VignetteInk.dim, opacity: 1)

        let go = VignetteMotion.ease(t, 1.0, 1.4)
        let close = VignetteMotion.ease(t, 1.4, 1.8)
        // What the sweep left fades out with the receipt, then the poster's
        // list fades back in and rests before the seam.
        let out = VignetteMotion.ease(t, 3.4, 3.7)
        let back = VignetteMotion.ease(t, 3.8, 4.2)

        if t < 3.75 {
            // The rows that stay: row 0 holds, row 2 rises into row 1's place.
            VignetteParts.miniMail(&ctx, rect: row(slots[0]), design: 0, opacity: 1 - out)
            VignetteParts.miniMail(
                &ctx, rect: row(VignetteMotion.mix(slots[2], slots[1], close)), design: 2, opacity: 1 - out)
            // The stale two go together, out to the right, as one sweep.
            for i in stale.sorted() {
                var r = row(slots[i])
                r.origin.x += 24 * CGFloat(go)
                VignetteParts.miniMail(&ctx, rect: r, design: i, opacity: 0.45 * (1 - go))
            }
        } else {
            for (i, y) in slots.enumerated() {
                VignetteParts.miniMail(
                    &ctx, rect: row(y), design: i, opacity: (stale.contains(i) ? 0.45 : 1) * back)
            }
        }

        // The receipt: what was cleared, with a check.
        let shown = VignetteMotion.ease(t, 1.9, 2.2) * (1 - out)
        VignetteParts.cardGround(&ctx, rect: receipt, cornerRadius: 5, opacity: shown)
        VignetteParts.check(
            &ctx, center: CGPoint(x: receipt.minX + 10, y: receipt.minY + 10),
            draw: VignetteMotion.ease(t, 2.05, 2.25), opacity: shown, size: 0.7)
        VignetteParts.bar(
            &ctx, CGRect(x: receipt.minX + 18, y: receipt.minY + 8.5, width: 16, height: 3),
            color: VignetteInk.ink.opacity(0.8), opacity: shown)
        for (i, w) in [CGFloat(26), 20].enumerated() {
            VignetteParts.bar(
                &ctx, CGRect(x: receipt.minX + 6, y: receipt.minY + 17 + 5 * CGFloat(i), width: w, height: 2.4),
                color: VignetteInk.dim, opacity: shown)
        }

        drawPill(&ctx, t: t)
    }

    private static func row(_ y: CGFloat) -> CGRect {
        CGRect(x: x, y: y, width: width, height: height)
    }

    /// The pill, pressed like a keycap (in, lit, sunk a unit), but it stays
    /// lit while the sweep works rather than springing straight back.
    private static func drawPill(_ ctx: inout GraphicsContext, t: Double) {
        let lit = VignetteMotion.ease(t, press, press + 0.06) * (1 - VignetteMotion.ease(t, 1.4, 1.7))
        let sink = VignetteMotion.press(t, at: press)
        let rect = pill.offsetBy(dx: 0, dy: CGFloat(sink))
        let shape = Path(roundedRect: rect, cornerRadius: rect.height / 2, style: .continuous)
        ctx.fill(shape, with: .color(VignetteInk.cap))
        ctx.stroke(shape, with: .color(VignetteInk.edge), lineWidth: 0.6)
        if lit > 0.001 {
            var glow = ctx
            glow.opacity = lit
            glow.addFilter(.shadow(color: VignetteInk.phosphor.opacity(0.7), radius: 6))
            glow.fill(shape, with: .color(VignetteInk.phosphor))
        }
        // The pill's symbol (a small stack) and its label, as bars.
        let ink = lit > 0.5 ? Color.white : VignetteInk.ink
        for i in 0..<2 {
            VignetteParts.bar(
                &ctx, CGRect(x: rect.minX + 6, y: rect.minY + 4 + 3 * CGFloat(i), width: 7, height: 1.6),
                color: ink, opacity: 0.85)
        }
        VignetteParts.bar(
            &ctx, CGRect(x: rect.minX + 17, y: rect.midY - 1.3, width: 21, height: 2.6),
            color: ink, opacity: 0.85)
    }
}
