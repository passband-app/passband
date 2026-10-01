// THE VIGNETTES — one small looping picture per feature set, in the discovery
// menu's tiles and in the found moment. Each one DEMONSTRATES its gesture: not
// a screenshot, not a video, an abstract little night screen where mail moves
// the way the keys move it.
//
// THE LANGUAGE (every vignette is held to this; `flow` is the reference):
//
//   CANVAS. 168 x 96 design units (7:4). Draw in those units only; the stage
//   scales the context to whatever size it is given, so a vignette is the same
//   picture in a 178pt tile, a 98pt moment and under window zoom. Keep 8 units
//   clear at every edge, and the bottom-left 72 x 24 clear of anything that
//   matters: the menu tile's FOUND / A HABIT tag sits there.
//
//   GROUND. Always night, in BOTH themes: `VignetteInk.groundTop` to
//   `groundBottom`, top to bottom, with an even 0.5pt `edge` border. It is a
//   screen inside the app, like the iPhone ad's phone, and a dark screen on a
//   light glass card reads as a product shot rather than as a dark-mode leak.
//
//   PALETTE. Only `VignetteInk`: `phosphor` (the one actor colour, the thing
//   the key acts on or the key itself), `phosphorHot` (a press, a check, a
//   completion), `ink` / `dim` / `faint` for mail and furniture, and the three
//   `avatar` tints for telling one email from the next. No tier colours (red,
//   amber, green): this is not the board, and colour here never means urgency.
//
//   MAIL. An email is a rounded card (radius 7) of `faint` fill and `edge`
//   border holding an avatar dot (r 5), a title bar (h 3.5, `ink` 0.8) and a
//   sub bar (h 3, `dim`). Bars, never letters: no text, no digits, no real
//   names anywhere in a vignette. A stack behind it steps 7 then 6 units down
//   at 0.93 and 0.86 scale.
//
//   KEYCAPS. `VignetteKeycap`: 22 units tall, radius 5, `cap` fill, even `edge`
//   border, label in SF Mono medium 10 at `ink` 0.85. Pressed, the cap fills
//   `phosphor`, its label goes white, it sinks 1 unit and glows (shadow r 6,
//   even all round). A press is 0.06s down, held to 0.18s, up by 0.26s. Show
//   ONLY the keys the set's tile lists, labelled exactly as the tile does.
//
//   STROKES. 1.5 units for marks (a check, an underline), 0.5pt for borders.
//   Round caps and joins.
//
//   LOOP. 4.8 seconds, built from beats: rest, gesture, rest. A loop BEGINS
//   AND ENDS ON THE SAME FRAME (the poster), with at least 0.6s of stillness
//   at the seam, so the repeat is invisible. If a loop moves N cards through
//   a deck, give the deck N distinct card designs so it comes back to itself.
//   Write the vignette as a PURE function of `t` in 0..<loop; no @State.
//
//   MOTION. Translate, fade, a scale of at most 0.93...1.05, a stroke drawing
//   on. Ease in-out (`VignetteMotion.ease`). No bounce past 1, no rotation, no
//   blur, no particles, no confetti. One thing moves at a time.
//
//   POSTER. `posterTime`: the frame that best explains the gesture still,
//   usually mid-gesture (the key down, the mark drawn). Reduce Motion, a
//   window that is not key and a stage off screen all show it, and an off
//   screen stage stops its timeline entirely.
//
// THE OTHER SEVEN are `PlaceholderVignette` until they are drawn: the set's
// symbol breathing over a faint stack, on the same ground. Replace one by
// adding a case to `DiscoveryVignette.scene` and a `VignetteScene` beside
// `FlowVignette`.

import SwiftUI

// MARK: - tokens

enum VignetteInk {
    static let groundTop = Color(hex: 0x111B2A)
    static let groundBottom = Color(hex: 0x080C14)
    static let edge = Color.white.opacity(0.09)
    static let phosphor = Color(hex: 0x4E9BEA)
    static let phosphorHot = Color(hex: 0x9CCBFF)
    static let ink = Color(hex: 0xE9EEF5)
    static let dim = Color(hex: 0x808E9F).opacity(0.55)
    static let faint = Color.white.opacity(0.06)
    static let cap = Color(hex: 0x1A2433)
    static let avatar: [Color] = [
        Color(hex: 0x4E9BEA), Color(hex: 0x9B8CF5), Color(hex: 0xD3C0A6),
    ]
}

enum VignetteMotion {
    static let canvas = CGSize(width: 168, height: 96)
    static let loop: Double = 4.8

    /// Smoothstep of `t` across `from...to`, clamped: 0 before, 1 after.
    static func ease(_ t: Double, _ from: Double, _ to: Double) -> Double {
        let x = min(1, max(0, (t - from) / (to - from)))
        return x * x * (3 - 2 * x)
    }

    /// How far down a key is, 0...1, for a press that starts at `at`.
    static func press(_ t: Double, at: Double) -> Double {
        let d = t - at
        if d < 0 || d > 0.26 { return 0 }
        if d < 0.06 { return d / 0.06 }
        if d < 0.18 { return 1 }
        return 1 - (d - 0.18) / 0.08
    }

    static func mix(_ a: CGFloat, _ b: CGFloat, _ p: Double) -> CGFloat { a + (b - a) * CGFloat(p) }
}

/// One vignette: a pure drawing of time.
protocol VignetteScene {
    static var posterTime: Double { get }
    static func draw(_ ctx: inout GraphicsContext, t: Double, set: FeatureSet)
}

// MARK: - the stage

/// The ground, the clock and the pause rules every vignette shares.
struct DiscoveryVignette: View {
    let set: FeatureSet
    /// False shows the poster: the window is not key, or the surface around
    /// the stage is not settled yet.
    var live = true
    /// Pins the clock, for the harness that captures frames.
    var fixedTime: Double? = nil
    var cornerRadius: CGFloat = 12

    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var onScreen = false

    private var running: Bool { live && onScreen && !reduceMotion && fixedTime == nil }

    var body: some View {
        // Paused, the timeline draws once and stops asking for frames.
        TimelineView(.animation(minimumInterval: 1 / 30, paused: !running)) { timeline in
            let t = time(timeline.date)
            Canvas { ctx, size in
                let scale = size.width / VignetteMotion.canvas.width
                ctx.scaleBy(x: scale, y: scale)
                drawGround(&ctx)
                scene.draw(&ctx, t: t, set: set)
            }
        }
        .aspectRatio(VignetteMotion.canvas.width / VignetteMotion.canvas.height, contentMode: .fit)
        .clipShape(RoundedRectangle(cornerRadius: cornerRadius, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: cornerRadius, style: .continuous)
                .strokeBorder(VignetteInk.edge, lineWidth: 0.5))
        .onAppear { onScreen = true }
        .onDisappear { onScreen = false }
        .accessibilityHidden(true)
    }

    private var scene: any VignetteScene.Type {
        switch set {
        case .flow: FlowVignette.self
        default: PlaceholderVignette.self
        }
    }

    private func time(_ date: Date) -> Double {
        if let fixedTime { return fixedTime }
        guard running else { return scene.posterTime }
        return date.timeIntervalSinceReferenceDate.truncatingRemainder(dividingBy: VignetteMotion.loop)
    }

    private func drawGround(_ ctx: inout GraphicsContext) {
        let rect = CGRect(origin: .zero, size: VignetteMotion.canvas)
        ctx.fill(
            Path(rect),
            with: .linearGradient(
                Gradient(colors: [VignetteInk.groundTop, VignetteInk.groundBottom]),
                startPoint: .zero, endPoint: CGPoint(x: 0, y: rect.height)))
        // A wide, low phosphor bloom from below centre: the brand's light,
        // still, so the screen is lit rather than black.
        ctx.fill(
            Path(rect),
            with: .radialGradient(
                Gradient(colors: [VignetteInk.phosphor.opacity(0.16), .clear]),
                center: CGPoint(x: rect.midX, y: rect.maxY + 10),
                startRadius: 0, endRadius: rect.width * 0.62))
    }
}

// MARK: - shared parts

enum VignetteParts {
    /// One email card, top-left at `origin`, of design `design` (0..<3).
    static func mail(
        _ ctx: inout GraphicsContext, origin: CGPoint, width: CGFloat = 98, design: Int,
        opacity: Double, scale: CGFloat = 1
    ) {
        let designs: [(title: CGFloat, sub: CGFloat)] = [(44, 60), (34, 52), (50, 38)]
        let d = designs[design % designs.count]
        let w = width * scale
        let h: CGFloat = 30 * scale
        let rect = CGRect(x: origin.x + (width - w) / 2, y: origin.y, width: w, height: h)
        var layer = ctx
        layer.opacity = opacity
        let card = Path(roundedRect: rect, cornerRadius: 7 * scale, style: .continuous)
        layer.fill(card, with: .color(Color(hex: 0x1B2638)))
        layer.fill(card, with: .color(VignetteInk.faint))
        layer.stroke(card, with: .color(VignetteInk.edge), lineWidth: 0.6)
        let avatar = CGRect(x: rect.minX + 8 * scale, y: rect.midY - 5 * scale, width: 10 * scale, height: 10 * scale)
        layer.fill(Path(ellipseIn: avatar), with: .color(VignetteInk.avatar[design % 3].opacity(0.85)))
        let x = avatar.maxX + 6 * scale
        layer.fill(
            Path(roundedRect: CGRect(x: x, y: rect.minY + 9.5 * scale, width: d.title * scale, height: 3.5 * scale), cornerRadius: 1.75 * scale),
            with: .color(VignetteInk.ink.opacity(0.8)))
        layer.fill(
            Path(roundedRect: CGRect(x: x, y: rect.minY + 17 * scale, width: d.sub * scale, height: 3 * scale), cornerRadius: 1.5 * scale),
            with: .color(VignetteInk.dim))
    }

    /// A keycap, top-left at `origin`.
    static func keycap(
        _ ctx: inout GraphicsContext, _ label: String, origin: CGPoint, width: CGFloat = 22, pressed: Double
    ) {
        let rect = CGRect(x: origin.x, y: origin.y + CGFloat(pressed), width: width, height: 22)
        let cap = Path(roundedRect: rect, cornerRadius: 5, style: .continuous)
        var layer = ctx
        if pressed > 0 {
            layer.addFilter(.shadow(color: VignetteInk.phosphor.opacity(0.75 * pressed), radius: 6))
        }
        layer.fill(cap, with: .color(VignetteInk.cap))
        layer.fill(cap, with: .color(VignetteInk.phosphor.opacity(0.95 * pressed)))
        ctx.stroke(cap, with: .color(VignetteInk.edge.opacity(1 - 0.6 * pressed)), lineWidth: 0.6)
        let text = Text(label)
            .font(.system(size: 10, weight: .medium, design: .monospaced))
            .foregroundStyle(pressed > 0.5 ? Color.white : VignetteInk.ink.opacity(0.85))
        ctx.draw(text, at: CGPoint(x: rect.midX, y: rect.midY), anchor: .center)
    }
}

// MARK: - flow, the reference

/// FLOW MODE: mail going past one at a time. Two `E`s each finish the email in
/// front (a check draws on, the card lifts away through the top, the next one
/// steps up), then a `⇧S` sets one aside to the left, unfinished and unjudged.
/// Three moves, three card designs, so the deck is back where it began.
enum FlowVignette: VignetteScene {
    private enum Kind { case done, skip }
    private static let events: [(at: Double, kind: Kind)] = [
        (0.7, .done), (2.1, .done), (3.5, .skip),
    ]
    /// An event's whole span: press, mark, departure, the deck stepping up.
    private static let span: Double = 0.84

    /// `E` held down and the check just drawn, the card still in place: the
    /// gesture in one frame.
    static let posterTime: Double = 0.86

    private static let deckOrigin = CGPoint(x: 16, y: 26)
    /// Depth poses: offset down, scale, opacity, for the card in front (0)
    /// and the three behind it. Depth 3 is invisible: where a new card waits.
    private static let poseY: [CGFloat] = [0, 7, 13, 18]
    private static let poseScale: [CGFloat] = [1, 0.93, 0.86, 0.8]
    private static let poseOpacity: [Double] = [1, 0.55, 0.26, 0]

    static func draw(_ ctx: inout GraphicsContext, t: Double, set: FeatureSet) {
        // The lane: a short phosphor rule over the deck, For Your Eyes' header.
        ctx.fill(
            Path(roundedRect: CGRect(x: deckOrigin.x, y: 13, width: 24, height: 3), cornerRadius: 1.5),
            with: .color(VignetteInk.phosphor.opacity(0.7)))
        ctx.fill(
            Path(roundedRect: CGRect(x: deckOrigin.x + 28, y: 13, width: 12, height: 3), cornerRadius: 1.5),
            with: .color(VignetteInk.dim))

        let finished = events.filter { t >= $0.at + span }.count
        let active = events.first { t >= $0.at && t < $0.at + span }
        let local = active.map { t - $0.at } ?? 0
        let depart = active == nil ? 0 : VignetteMotion.ease(local, 0.30, 0.62)
        let promote = active == nil ? 0 : VignetteMotion.ease(local, 0.42, 0.82)

        // Back to front, so the card in front is drawn last.
        for depth in stride(from: 3, through: 1, by: -1) {
            let d = Double(depth) - promote
            let lo = Int(d.rounded(.down)), hi = min(3, lo + 1)
            let f = d - Double(lo)
            let y = VignetteMotion.mix(poseY[lo], poseY[hi], f)
            let s = VignetteMotion.mix(poseScale[lo], poseScale[hi], f)
            let o = poseOpacity[lo] + (poseOpacity[hi] - poseOpacity[lo]) * f
            VignetteParts.mail(
                &ctx, origin: CGPoint(x: deckOrigin.x, y: deckOrigin.y + y),
                design: (finished + depth) % 3, opacity: o, scale: s)
        }

        // The card in front, leaving the way its key sends it.
        let kind = active?.kind ?? .done
        let front = CGPoint(
            x: deckOrigin.x - (kind == .skip ? 26 * depart : 0),
            y: deckOrigin.y - (kind == .done ? 24 * depart : 0))
        VignetteParts.mail(&ctx, origin: front, design: finished % 3, opacity: 1 - depart)

        // Done draws its check on the card before the card goes.
        if kind == .done, active != nil {
            let draw = VignetteMotion.ease(local, 0.03, 0.16)
            var check = Path()
            let cx = front.x + 84, cy = front.y + 15
            check.move(to: CGPoint(x: cx - 5, y: cy))
            check.addLine(to: CGPoint(x: cx - 1.5, y: cy + 3.5))
            check.addLine(to: CGPoint(x: cx + 5, y: cy - 4))
            var layer = ctx
            layer.opacity = 1 - depart
            layer.addFilter(.shadow(color: VignetteInk.phosphor.opacity(0.8), radius: 4))
            layer.stroke(
                check.trimmedPath(from: 0, to: draw),
                with: .color(VignetteInk.phosphorHot),
                style: StrokeStyle(lineWidth: 1.6, lineCap: .round, lineJoin: .round))
        }

        // The keys, right of the deck, in the order the tile lists them.
        let doneDown = events.filter { $0.kind == .done }.map { VignetteMotion.press(t, at: $0.at) }.max() ?? 0
        let skipDown = events.filter { $0.kind == .skip }.map { VignetteMotion.press(t, at: $0.at) }.max() ?? 0
        VignetteParts.keycap(&ctx, "E", origin: CGPoint(x: 128, y: 22), pressed: doneDown)
        VignetteParts.keycap(&ctx, "⇧S", origin: CGPoint(x: 124, y: 52), width: 30, pressed: skipDown)
    }
}

// MARK: - the placeholder

/// The other seven, until each gets its own: the set's symbol, breathing, over
/// a faint stack. Honest about being a placeholder by doing very little.
enum PlaceholderVignette: VignetteScene {
    static let posterTime: Double = 1.2

    static func draw(_ ctx: inout GraphicsContext, t: Double, set: FeatureSet) {
        let breath = 0.5 - 0.5 * cos(t / VignetteMotion.loop * 2 * .pi)
        for depth in stride(from: 2, through: 0, by: -1) {
            VignetteParts.mail(
                &ctx, origin: CGPoint(x: 16, y: 30 + CGFloat(depth) * 7), width: 82,
                design: depth, opacity: [0.32, 0.2, 0.1][depth], scale: [1, 0.93, 0.86][depth])
        }
        let center = CGPoint(x: 126, y: 48)
        var halo = ctx
        halo.fill(
            Path(ellipseIn: CGRect(x: center.x - 22, y: center.y - 22, width: 44, height: 44)),
            with: .radialGradient(
                Gradient(colors: [VignetteInk.phosphor.opacity(0.22 + 0.16 * breath), .clear]),
                center: center, startRadius: 0, endRadius: 22))
        var glyph = ctx
        glyph.addFilter(.shadow(color: VignetteInk.phosphor.opacity(0.7), radius: 5 + 2 * breath))
        let symbol = Text(Image(systemName: set.symbol))
            .font(.system(size: 22, weight: .light))
            .foregroundStyle(VignetteInk.phosphorHot)
        glyph.draw(symbol, at: center, anchor: .center)
    }
}
