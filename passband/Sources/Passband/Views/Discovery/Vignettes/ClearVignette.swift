import SwiftUI

/// THE BASICS: one email on a board of three. `e` checks it and it slides
/// away, the row below closing the gap; `u` opens the gap again and the same
/// email slides back into its place, lit for a moment. Not a deck walked
/// through (that is flow): one thing, gone and returned, nothing lost.
enum ClearVignette: VignetteScene {
    private static let pressDone: Double = 0.7
    private static let pressUndo: Double = 2.5

    /// `e` held down and the check drawn on the middle row, still in place.
    static let posterTime: Double = 0.86

    private static let x: CGFloat = 16
    private static let width: CGFloat = 100
    private static let slots: [CGFloat] = [19, 38, 57]

    static func draw(_ ctx: inout GraphicsContext, t: Double, set: FeatureSet) {
        // The lane header, as in flow: this is the board.
        ctx.fill(
            Path(roundedRect: CGRect(x: x, y: 10, width: 24, height: 3), cornerRadius: 1.5),
            with: .color(VignetteInk.phosphor.opacity(0.7)))
        ctx.fill(
            Path(roundedRect: CGRect(x: x + 28, y: 10, width: 12, height: 3), cornerRadius: 1.5),
            with: .color(VignetteInk.dim))

        let away = VignetteMotion.ease(t, 1.0, 1.36) - VignetteMotion.ease(t, 2.84, 3.24)
        let close = VignetteMotion.ease(t, 1.24, 1.62) - VignetteMotion.ease(t, 2.6, 2.96)

        // The rows that stay.
        VignetteParts.row(&ctx, origin: CGPoint(x: x, y: slots[0]), width: width, design: 0, opacity: 1)
        VignetteParts.row(
            &ctx, origin: CGPoint(x: x, y: VignetteMotion.mix(slots[2], slots[1], close)),
            width: width, design: 2, opacity: 1)

        // The one that goes and comes back: out to the right, and back the
        // same way, so it reads as kept nearby rather than destroyed.
        let origin = CGPoint(x: x + 26 * CGFloat(away), y: slots[1])
        VignetteParts.row(&ctx, origin: origin, width: width, design: 1, opacity: 1 - away)
        let rect = CGRect(x: origin.x, y: origin.y, width: width, height: 30 * 0.56)
        if t < 2 {
            VignetteParts.check(
                &ctx, center: CGPoint(x: rect.maxX - 10, y: rect.midY),
                draw: VignetteMotion.ease(t, pressDone + 0.03, pressDone + 0.16),
                opacity: 1 - away, size: 0.8)
        }
        let lit = VignetteMotion.ease(t, 3.14, 3.34) * (1 - VignetteMotion.ease(t, 3.55, 4.1))
        VignetteParts.halo(&ctx, rect: rect, cornerRadius: 7 * 0.56, amount: lit)

        VignetteParts.keycap(&ctx, "e", origin: CGPoint(x: 128, y: 22), pressed: VignetteMotion.press(t, at: pressDone))
        VignetteParts.keycap(&ctx, "u", origin: CGPoint(x: 128, y: 52), pressed: VignetteMotion.press(t, at: pressUndo))
    }
}
