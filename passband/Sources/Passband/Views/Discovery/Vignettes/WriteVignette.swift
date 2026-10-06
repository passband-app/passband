import SwiftUI

/// WRITING: `c` opens the composer, addressed to a group (three dots, three
/// people). A short message is written, bars drawing on behind a caret. Then
/// it goes, and one message becomes three: copies fan out to the right, one
/// for each person, and land lit. Then the picture rests and clears.
enum WriteVignette: VignetteScene {
    private static let press: Double = 0.5

    /// Sent: the group in the To line, the message dimmed behind it, and its
    /// three copies landed and lit.
    static let posterTime: Double = 3.05

    private static let panel = CGRect(x: 16, y: 12, width: 72, height: 50)
    private static let lines: [(width: CGFloat, from: Double, to: Double)] = [
        (54, 0.88, 1.28), (46, 1.34, 1.7), (28, 1.76, 1.98),
    ]
    private static let launch = CGRect(x: 30, y: 31, width: 30, height: 11)
    private static func landing(_ i: Int) -> CGRect {
        CGRect(x: 96, y: 17 + CGFloat(i) * 16, width: 30, height: 11)
    }

    static func draw(_ ctx: inout GraphicsContext, t: Double, set: FeatureSet) {
        let reset = VignetteMotion.ease(t, 3.8, 4.15)
        let keep = 1 - reset

        // The composer, opened by `c`.
        let open = VignetteMotion.ease(t, press + 0.06, press + 0.3) * keep
        let rect = panel.offsetBy(dx: 0, dy: 4 * CGFloat(1 - VignetteMotion.ease(t, press + 0.06, press + 0.3)))
        VignetteParts.cardGround(&ctx, rect: rect, cornerRadius: 7, opacity: open)
        if open > 0.001 {
            var lit = ctx
            lit.opacity = open
            lit.addFilter(.shadow(color: VignetteInk.phosphor.opacity(0.5), radius: 4))
            lit.stroke(
                Path(roundedRect: rect, cornerRadius: 7, style: .continuous),
                with: .color(VignetteInk.phosphor.opacity(0.6)), lineWidth: 0.8)

            // To: a group, three people in one chip.
            var layer = ctx
            layer.opacity = open
            let chip = CGRect(x: rect.minX + 6, y: rect.minY + 5, width: 30, height: 10)
            layer.fill(Path(roundedRect: chip, cornerRadius: 5, style: .continuous), with: .color(VignetteInk.faint))
            layer.stroke(Path(roundedRect: chip, cornerRadius: 5, style: .continuous), with: .color(VignetteInk.edge), lineWidth: 0.6)
            for i in (0..<3).reversed() {
                let c = CGRect(x: chip.minX + 2.5 + CGFloat(i) * 5, y: chip.midY - 3, width: 6, height: 6)
                layer.fill(Path(ellipseIn: c.insetBy(dx: -0.6, dy: -0.6)), with: .color(Color(hex: 0x1F2A3B)))
                layer.fill(Path(ellipseIn: c), with: .color(VignetteInk.avatar[i].opacity(0.9)))
            }
            layer.fill(
                Path(roundedRect: CGRect(x: chip.minX + 20, y: chip.midY - 1, width: 7, height: 2), cornerRadius: 1),
                with: .color(VignetteInk.ink.opacity(0.55)))
            layer.fill(
                Path(CGRect(x: rect.minX + 6, y: rect.minY + 19, width: rect.width - 12, height: 0.5)),
                with: .color(VignetteInk.edge))
        }

        // The message: bars drawn on behind a caret, dimmed once it is sent.
        let sent = VignetteMotion.ease(t, 2.1, 2.26)
        var caretX = rect.minX + 6, caretY = rect.minY + 25.5
        for (i, line) in lines.enumerated() {
            let w = line.width * CGFloat(VignetteMotion.ease(t, line.from, line.to))
            let y = rect.minY + 24 + CGFloat(i) * 7
            VignetteParts.bar(
                &ctx, CGRect(x: rect.minX + 6, y: y, width: w, height: 3),
                color: VignetteInk.ink.opacity(0.75), opacity: open * (1 - 0.7 * sent))
            if t >= line.from { caretX = rect.minX + 6 + w + 2; caretY = y + 1.5 }
        }
        let caret = VignetteMotion.ease(t, press + 0.24, press + 0.34) * (1 - VignetteMotion.ease(t, 2.0, 2.12))
        VignetteParts.bar(
            &ctx, CGRect(x: caretX, y: caretY - 4, width: 1.5, height: 8),
            color: VignetteInk.phosphorHot, opacity: caret * keep)

        // Sent: the message gathers into one card, which becomes three, one
        // for each person, fanning out to the right.
        let fan = VignetteMotion.ease(t, 2.42, 2.9)
        let gathered = VignetteMotion.ease(t, 2.24, 2.4) * keep
        for i in 0..<3 {
            let to = landing(i)
            let r = CGRect(
                x: VignetteMotion.mix(launch.minX, to.minX, fan), y: VignetteMotion.mix(launch.minY, to.minY, fan),
                width: to.width, height: to.height)
            VignetteParts.miniMail(&ctx, rect: r, design: i, opacity: gathered)
            let lit = VignetteMotion.ease(t, 2.84, 3.0) * (1 - VignetteMotion.ease(t, 3.4, 3.8))
            VignetteParts.halo(&ctx, rect: r, cornerRadius: 3.5, amount: lit * keep)
        }

        VignetteParts.keycap(&ctx, "c", origin: CGPoint(x: 136, y: 17), pressed: VignetteMotion.press(t, at: press))
        VignetteParts.keycap(&ctx, "r", origin: CGPoint(x: 136, y: 47), pressed: 0)
    }
}
