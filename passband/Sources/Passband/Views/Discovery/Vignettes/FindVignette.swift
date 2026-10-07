import SwiftUI

/// FIND ANYTHING: a field of nine emails. `/` lights the search field; four
/// strokes are typed (ticks, not letters), and with each one the emails that
/// do not match fade back until one is left. It steps to the front and opens
/// into a whole row, lit: found. Then the field lets go and the nine return.
enum FindVignette: VignetteScene {
    private static let press: Double = 0.6
    private static let ticks: [Double] = [1.0, 1.25, 1.5, 1.75]
    /// Which cards each tick rules out; the one never named is the match.
    private static let ruledOut: [[Int]] = [[0, 4, 8], [2, 6], [1, 7], [3]]
    private static let match = 5

    /// Typed, narrowed, the match at the front and lit.
    static let posterTime: Double = 2.9

    private static let field = CGRect(x: 16, y: 10, width: 100, height: 14)

    private static func design(_ i: Int) -> Int { (i + i / 3) % 3 }

    private static func cell(_ i: Int) -> CGRect {
        CGRect(x: 16 + CGFloat(i % 3) * 35, y: 31 + CGFloat(i / 3) * 14, width: 30, height: 11)
    }

    static func draw(_ ctx: inout GraphicsContext, t: Double, set: FeatureSet) {
        let open = VignetteMotion.ease(t, press + 0.06, press + 0.22) * (1 - VignetteMotion.ease(t, 3.6, 3.85))

        // The search field: always there faintly, lit by `/`.
        let pill = Path(roundedRect: field, cornerRadius: field.height / 2, style: .continuous)
        ctx.fill(pill, with: .color(VignetteInk.faint))
        ctx.stroke(pill, with: .color(VignetteInk.edge), lineWidth: 0.6)
        if open > 0.001 {
            var lit = ctx
            lit.opacity = open
            lit.addFilter(.shadow(color: VignetteInk.phosphor.opacity(0.6), radius: 4))
            lit.stroke(pill, with: .color(VignetteInk.phosphor.opacity(0.85)), lineWidth: 0.9)
        }
        // A lens mark at the field's start: a ring and a handle, strokes only.
        var lens = Path()
        lens.addEllipse(in: CGRect(x: field.minX + 6, y: field.midY - 3.2, width: 5.4, height: 5.4))
        lens.move(to: CGPoint(x: field.minX + 10.6, y: field.midY + 1.6))
        lens.addLine(to: CGPoint(x: field.minX + 12.6, y: field.midY + 3.6))
        ctx.stroke(lens, with: .color(VignetteInk.ink.opacity(0.45)), style: StrokeStyle(lineWidth: 1, lineCap: .round))

        // The strokes typed, and the caret after them.
        var typed = 0
        for (i, at) in ticks.enumerated() {
            let a = VignetteMotion.ease(t, at, at + 0.08) * open
            guard a > 0.001 else { continue }
            if t >= at { typed = i + 1 }
            var tick = ctx
            tick.opacity = a
            tick.fill(
                Path(roundedRect: CGRect(x: field.minX + 19 + CGFloat(i) * 5, y: field.midY - 2.5, width: 2, height: 5), cornerRadius: 1),
                with: .color(VignetteInk.ink.opacity(0.85)))
        }
        if open > 0.001 {
            var caret = ctx
            caret.opacity = open
            caret.fill(
                Path(roundedRect: CGRect(x: field.minX + 19 + CGFloat(typed) * 5, y: field.midY - 4, width: 1.5, height: 8), cornerRadius: 0.75),
                with: .color(VignetteInk.phosphorHot))
        }

        // The field of mail, narrowing. Designs mixed so no column is one colour.
        let result = VignetteMotion.ease(t, 2.25, 2.55) * (1 - VignetteMotion.ease(t, 3.6, 3.85))
        let returnAll = VignetteMotion.ease(t, 3.92, 4.22)
        for i in 0..<9 where i != match {
            let k = ruledOut.firstIndex { $0.contains(i) } ?? 0
            let fade = VignetteMotion.ease(t, ticks[k] + 0.04, ticks[k] + 0.26) * (1 - returnAll)
            VignetteParts.miniMail(&ctx, rect: cell(i), design: design(i), opacity: (1 - 0.88 * fade) * (1 - result))
        }

        // The match steps to the front and opens out into a whole row: the
        // one you were looking for, lit.
        let front = VignetteMotion.ease(t, 2.1, 2.45) * (1 - VignetteMotion.ease(t, 3.66, 4.0))
        let from = cell(match), to = cell(0)
        let rect = CGRect(
            x: VignetteMotion.mix(from.minX, to.minX, front), y: VignetteMotion.mix(from.minY, to.minY, front),
            width: from.width, height: from.height)
        VignetteParts.miniMail(&ctx, rect: rect, design: design(match), opacity: 1 - result)
        let row = CGRect(x: 16, y: 30, width: 100, height: 30 * 0.56)
        VignetteParts.row(&ctx, origin: row.origin, width: row.width, design: design(match), opacity: result)
        let lit = VignetteMotion.ease(t, 2.45, 2.65) * (1 - VignetteMotion.ease(t, 3.45, 3.7))
        VignetteParts.halo(&ctx, rect: row, cornerRadius: 7 * 0.56, amount: lit)

        VignetteParts.keycap(&ctx, "/", origin: CGPoint(x: 128, y: 22), pressed: VignetteMotion.press(t, at: press))
        VignetteParts.keycap(&ctx, "⌘K", origin: CGPoint(x: 124, y: 52), width: 30, pressed: 0)
    }
}
