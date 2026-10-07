import SwiftUI

/// AT A GLANCE: two records on the board that carry their answer on their
/// face, so nothing is opened. A sign-in code sits in its row as a chip of
/// blocks; `c` lifts it and a copy rises away: copied. A package's track
/// runs on to its last stop and a check lands there: delivered. `e` and the
/// record lifts off the board, done. Then it comes back, still on its way.
enum GlanceVignette: VignetteScene {
    private static let pressCopy: Double = 0.7
    private static let pressDone: Double = 3.0

    /// `c` down, the code chip lifted and lit, its copy on the way up.
    static let posterTime: Double = 0.86

    private static let package = CGRect(x: 16, y: 21, width: 100, height: 22)
    private static let code = CGRect(x: 16, y: 50, width: 100, height: 22)

    static func draw(_ ctx: inout GraphicsContext, t: Double, set: FeatureSet) {
        // The lane header, as on the board.
        VignetteParts.bar(&ctx, CGRect(x: 16, y: 12, width: 24, height: 3), color: VignetteInk.phosphor, opacity: 0.7)
        VignetteParts.bar(&ctx, CGRect(x: 44, y: 12, width: 12, height: 3), color: VignetteInk.dim, opacity: 1)

        drawPackage(&ctx, t: t)
        drawCode(&ctx, t: t)

        VignetteParts.keycap(&ctx, "e", origin: CGPoint(x: 128, y: package.minY), pressed: VignetteMotion.press(t, at: pressDone))
        VignetteParts.keycap(&ctx, "c", origin: CGPoint(x: 128, y: code.minY), pressed: VignetteMotion.press(t, at: pressCopy))
    }

    /// The package: a track of four stops, already part way.
    private static func drawPackage(_ ctx: inout GraphicsContext, t: Double) {
        let leave = VignetteMotion.ease(t, pressDone + 0.12, pressDone + 0.46)
        let back = VignetteMotion.ease(t, 3.95, 4.3)
        let opacity = t < 3.7 ? 1 - leave : back
        guard opacity > 0.001 else { return }
        let rect = package.offsetBy(dx: 0, dy: t < 3.7 ? -10 * CGFloat(leave) : 0)

        VignetteParts.cardGround(&ctx, rect: rect, cornerRadius: 5, opacity: opacity)
        dot(&ctx, rect: rect, design: 2, opacity: opacity)
        VignetteParts.bar(
            &ctx, CGRect(x: rect.minX + 16, y: rect.minY + 5.5, width: 30, height: 3),
            color: VignetteInk.ink.opacity(0.8), opacity: opacity)

        let x0 = rect.minX + 17, x1 = rect.maxX - 9, y = rect.minY + 15
        let start = 1.0 / 3
        let p = t < 3.7 ? start + (1 - start) * VignetteMotion.ease(t, 1.7, 2.6) : start
        let head = VignetteMotion.mix(x0, x1, p)

        var base = Path()
        base.move(to: CGPoint(x: x0, y: y))
        base.addLine(to: CGPoint(x: x1, y: y))
        var layer = ctx
        layer.opacity = opacity
        layer.stroke(base, with: .color(VignetteInk.dim), style: StrokeStyle(lineWidth: 1, lineCap: .round))
        var run = Path()
        run.move(to: CGPoint(x: x0, y: y))
        run.addLine(to: CGPoint(x: head, y: y))
        var lit = ctx
        lit.opacity = opacity
        lit.addFilter(.shadow(color: VignetteInk.phosphor.opacity(0.6), radius: 3))
        lit.stroke(run, with: .color(VignetteInk.phosphor), style: StrokeStyle(lineWidth: 1.5, lineCap: .round))

        let delivered = t < 3.7 ? VignetteMotion.ease(t, 2.6, 2.78) : 0
        for i in 0..<4 {
            let f = Double(i) / 3
            let nx = VignetteMotion.mix(x0, x1, f)
            let passed = f <= p + 0.001
            if i == 3, delivered > 0 { continue }
            layer.fill(
                Path(ellipseIn: CGRect(x: nx - 1.8, y: y - 1.8, width: 3.6, height: 3.6)),
                with: .color(passed ? VignetteInk.phosphor : Color(hex: 0x3A4658)))
        }
        // The parcel itself, travelling: a hot point at the head.
        if delivered < 1 {
            var point = ctx
            point.opacity = opacity * (1 - delivered)
            point.addFilter(.shadow(color: VignetteInk.phosphor, radius: 4))
            point.fill(
                Path(ellipseIn: CGRect(x: head - 2.6, y: y - 2.6, width: 5.2, height: 5.2)),
                with: .color(VignetteInk.phosphorHot))
        }
        // Delivered: a check where the last stop was.
        VignetteParts.check(
            &ctx, center: CGPoint(x: x1 + 0.5, y: y - 0.5),
            draw: VignetteMotion.ease(t, 2.62, 2.8) * (t < 3.7 ? 1 : 0), opacity: opacity, size: 0.62)
    }

    /// The sign-in code: a chip of six blocks, two groups of three.
    private static func drawCode(_ ctx: inout GraphicsContext, t: Double) {
        let rect = code
        VignetteParts.cardGround(&ctx, rect: rect, cornerRadius: 5, opacity: 1)
        dot(&ctx, rect: rect, design: 1, opacity: 1)
        VignetteParts.bar(
            &ctx, CGRect(x: rect.minX + 16, y: rect.minY + 6.5, width: 22, height: 3),
            color: VignetteInk.ink.opacity(0.8), opacity: 1)
        VignetteParts.bar(
            &ctx, CGRect(x: rect.minX + 16, y: rect.minY + 12.5, width: 30, height: 2.6),
            color: VignetteInk.dim, opacity: 1)

        let lift = VignetteMotion.ease(t, pressCopy + 0.04, pressCopy + 0.18) * (1 - VignetteMotion.ease(t, 1.35, 1.65))
        let chip = CGRect(x: rect.minX + 58, y: rect.minY + 4.5 - 1.5 * CGFloat(lift), width: 34, height: 13)

        // The copy, rising off the chip and gone.
        let rise = VignetteMotion.ease(t, pressCopy + 0.1, pressCopy + 0.7)
        let ghost = VignetteMotion.ease(t, pressCopy + 0.06, pressCopy + 0.14) * (1 - VignetteMotion.ease(t, pressCopy + 0.35, pressCopy + 0.7))
        drawChip(&ctx, rect: chip.offsetBy(dx: 0, dy: -11 * CGFloat(rise)), opacity: 0.75 * ghost)

        drawChip(&ctx, rect: chip, opacity: 1)
        VignetteParts.halo(&ctx, rect: chip, cornerRadius: 3.5, amount: lift)
    }

    private static func drawChip(_ ctx: inout GraphicsContext, rect: CGRect, opacity: Double) {
        guard opacity > 0.001 else { return }
        var layer = ctx
        layer.opacity = opacity
        let pill = Path(roundedRect: rect, cornerRadius: 3.5, style: .continuous)
        layer.fill(pill, with: .color(Color(hex: 0x1B2638)))
        layer.fill(pill, with: .color(VignetteInk.phosphor.opacity(0.16)))
        layer.stroke(pill, with: .color(VignetteInk.phosphor.opacity(0.55)), lineWidth: 0.6)
        let w: CGFloat = 2.8, step: CGFloat = 4.3, gap: CGFloat = 3
        let total = 5 * step + gap + w
        let x0 = rect.midX - total / 2
        for i in 0..<6 {
            let x = x0 + CGFloat(i) * step + (i >= 3 ? gap : 0)
            layer.fill(
                Path(roundedRect: CGRect(x: x, y: rect.midY - 3, width: w, height: 6), cornerRadius: 0.9),
                with: .color(VignetteInk.ink.opacity(0.9)))
        }
    }

    private static func dot(_ ctx: inout GraphicsContext, rect: CGRect, design: Int, opacity: Double) {
        var layer = ctx
        layer.opacity = opacity
        layer.fill(
            Path(ellipseIn: CGRect(x: rect.minX + 5.5, y: rect.midY - 3.5, width: 7, height: 7)),
            with: .color(VignetteInk.avatar[design % 3].opacity(0.85)))
    }
}
