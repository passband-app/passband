// THE VIGNETTE STAGE: a small looping picture that DEMONSTRATES a gesture. Not
// a screenshot and not a video: an abstract little night screen where mail
// moves the way the keys move it. The What's New walk plays one per headliner.
//
// THE LANGUAGE (every scene is held to this):
//
//   CANVAS. 168 x 96 design units (7:4). Draw in those units only; the stage
//   scales the context to whatever size it is given, so a scene is the same
//   picture in a tile and across the top of a card. Keep 8 units clear at
//   every edge.
//
//   GROUND. Always night, in BOTH themes: `VignettePalette.groundTop` to
//   `groundBottom`. It is a screen inside the app, like the iPhone ad's phone,
//   and a dark screen on a light glass card reads as a product shot rather
//   than as a dark-mode leak.
//
//   PALETTE. Only `VignettePalette`: `phosphor` (the one actor colour, the
//   thing the key acts on or the key itself), `phosphorHot` (a press, a check,
//   a completion), `ink` / `dim` / `faint` for mail and furniture, and the
//   three `avatar` tints for telling one sender from the next. No tier colours
//   (red, amber, green): colour here never means urgency.
//
//   MAIL. An email is a rounded card of `faint` fill and `edge` border holding
//   an avatar dot, a title bar and a sub bar. Bars, never letters: no text, no
//   digits, no real names anywhere in a scene. The one exception is a keycap's
//   label, because the key IS the lesson.
//
//   KEYCAPS. `VignetteKit.keycap`: 22 units tall, even border. Pressed, the
//   cap fills `phosphor`, its label goes white, it sinks 1 unit and glows
//   evenly all round. Label a key exactly as the app's own hint does.
//
//   LOOP. 4.8 seconds, built from beats: rest, gesture, rest. A loop BEGINS
//   AND ENDS ON THE SAME FRAME, with stillness at the seam, so the repeat is
//   invisible. Write the scene as a PURE function of `t` in 0..<loop.
//
//   MOTION. Translate, fade, a scale of at most 0.93...1.05, a stroke drawing
//   on. No bounce past 1, no rotation, no blur, no particles. One thing moves
//   at a time.
//
//   POSTER. `posterTime`: the frame that best explains the gesture still.
//   Reduce Motion, a window that is not key and a stage off screen all show
//   it, and a paused stage stops asking for frames.
//
// The discovery ring (branch product-discovery) wrote this language first, with
// its own copy of the stage; when that lands it should draw on this one.

import SwiftUI

enum VignettePalette {
    static let groundTop = Color(hex: 0x111B2A)
    static let groundBottom = Color(hex: 0x080C14)
    static let edge = Color.white.opacity(0.09)
    static let phosphor = Color(hex: 0x4E9BEA)
    static let phosphorHot = Color(hex: 0x9CCBFF)
    static let ink = Color(hex: 0xE9EEF5)
    static let dim = Color(hex: 0x808E9F).opacity(0.55)
    static let faint = Color.white.opacity(0.06)
    static let card = Color(hex: 0x1B2638)
    static let cap = Color(hex: 0x1A2433)
    static let avatar: [Color] = [
        Color(hex: 0x4E9BEA), Color(hex: 0x9B8CF5), Color(hex: 0xD3C0A6),
    ]
}

enum VignetteClock {
    static let canvas = CGSize(width: 168, height: 96)
    static let loop: Double = 4.8

    /// Smoothstep of `t` across `from...to`, clamped: 0 before, 1 after.
    static func ease(_ t: Double, _ from: Double, _ to: Double) -> Double {
        let x = min(1, max(0, (t - from) / (to - from)))
        return x * x * (3 - 2 * x)
    }

    /// The same span with no easing, for a thing that is a timer.
    static func ramp(_ t: Double, _ from: Double, _ to: Double) -> Double {
        min(1, max(0, (t - from) / (to - from)))
    }

    /// How far down a key is, 0...1, for a tap that starts at `at`.
    static func press(_ t: Double, at: Double) -> Double {
        hold(t, from: at, to: at + 0.18)
    }

    /// How far down a key is for one held from `from` and let go at `to`.
    static func hold(_ t: Double, from: Double, to: Double) -> Double {
        if t < from || t > to + 0.08 { return 0 }
        if t < from + 0.06 { return (t - from) / 0.06 }
        if t <= to { return 1 }
        return 1 - (t - to) / 0.08
    }

    static func mix(_ a: CGFloat, _ b: CGFloat, _ p: Double) -> CGFloat { a + (b - a) * CGFloat(p) }
    static func mix(_ a: Double, _ b: Double, _ p: Double) -> Double { a + (b - a) * p }
}

/// One scene: a pure drawing of time.
protocol VignetteDrawing {
    static var posterTime: Double { get }
    static func draw(_ ctx: inout GraphicsContext, t: Double)
}

// MARK: - the stage

/// The ground, the clock and the pause rules every scene shares.
struct VignetteStage: View {
    let scene: any VignetteDrawing.Type
    /// False shows the poster: the window is not key, or the surface around
    /// the stage is not settled yet.
    var live = true
    /// Pins the clock, for a harness that captures frames.
    var fixedTime: Double? = nil
    /// 0 for a stage that bleeds to the edge of a card which clips it.
    var cornerRadius: CGFloat = 12

    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var onScreen = false

    private var running: Bool { live && onScreen && !reduceMotion && fixedTime == nil }

    var body: some View {
        // Paused, the timeline draws once and stops asking for frames.
        TimelineView(.animation(minimumInterval: 1 / 30, paused: !running)) { timeline in
            let t = time(timeline.date)
            Canvas { ctx, size in
                Self.render(&ctx, size: size, scene: scene, t: t)
            }
        }
        .aspectRatio(VignetteClock.canvas.width / VignetteClock.canvas.height, contentMode: .fit)
        .clipShape(RoundedRectangle(cornerRadius: cornerRadius, style: .continuous))
        .overlay {
            if cornerRadius > 0 {
                RoundedRectangle(cornerRadius: cornerRadius, style: .continuous)
                    .strokeBorder(VignettePalette.edge, lineWidth: 0.5)
            }
        }
        .onAppear { onScreen = true }
        .onDisappear { onScreen = false }
        .accessibilityHidden(true)
    }

    private func time(_ date: Date) -> Double {
        if let fixedTime { return fixedTime }
        guard running else { return scene.posterTime }
        return date.timeIntervalSinceReferenceDate.truncatingRemainder(dividingBy: VignetteClock.loop)
    }

    /// One frame, ground included. Static so a frame can be drawn with no view
    /// around it.
    static func render(_ ctx: inout GraphicsContext, size: CGSize, scene: any VignetteDrawing.Type, t: Double) {
        let scale = size.width / VignetteClock.canvas.width
        ctx.scaleBy(x: scale, y: scale)
        let rect = CGRect(origin: .zero, size: VignetteClock.canvas)
        ctx.fill(
            Path(rect),
            with: .linearGradient(
                Gradient(colors: [VignettePalette.groundTop, VignettePalette.groundBottom]),
                startPoint: .zero, endPoint: CGPoint(x: 0, y: rect.height)))
        // A wide, low phosphor bloom from below centre: the brand's light,
        // still, so the screen is lit rather than black.
        ctx.fill(
            Path(rect),
            with: .radialGradient(
                Gradient(colors: [VignettePalette.phosphor.opacity(0.16), .clear]),
                center: CGPoint(x: rect.midX, y: rect.maxY + 10),
                startRadius: 0, endRadius: rect.width * 0.62))
        scene.draw(&ctx, t: t)
    }
}

// MARK: - shared parts

enum VignetteKit {
    /// A rounded bar: the stand-in for a line of text.
    static func bar(_ ctx: inout GraphicsContext, _ rect: CGRect, _ color: Color) {
        ctx.fill(Path(roundedRect: rect, cornerRadius: rect.height / 2), with: .color(color))
    }

    /// One email card, top-left at `origin`. `design` (0..<3) picks the bar
    /// widths; `tint` picks the sender and defaults to the design's own, so a
    /// deck of three designs is three senders and a stack that passes one tint
    /// is one sender's mail. Opacities multiply the layer's own, so a part
    /// drawn into a fading layer fades with it.
    static func mail(
        _ ctx: inout GraphicsContext, origin: CGPoint, width: CGFloat = 98, design: Int,
        tint: Int? = nil, opacity: Double, scale: CGFloat = 1, lit: Double = 0, blank: Bool = false
    ) {
        let designs: [(title: CGFloat, sub: CGFloat)] = [(44, 60), (34, 52), (50, 38)]
        let d = designs[design % designs.count]
        let w = width * scale
        let h: CGFloat = 30 * scale
        let rect = CGRect(x: origin.x + (width - w) / 2, y: origin.y, width: w, height: h)
        var layer = ctx
        layer.opacity *= opacity
        let card = Path(roundedRect: rect, cornerRadius: 7 * scale, style: .continuous)
        layer.fill(card, with: .color(VignettePalette.card))
        layer.fill(card, with: .color(VignettePalette.faint))
        layer.stroke(card, with: .color(VignettePalette.edge), lineWidth: 0.6)
        if lit > 0 {
            layer.stroke(card, with: .color(VignettePalette.phosphor.opacity(0.85 * lit)), lineWidth: 0.8)
        }
        // A card deep in a fanned stack is an edge, not a second email to read.
        guard !blank else { return }
        let avatar = CGRect(
            x: rect.minX + 8 * scale, y: rect.midY - 5 * scale, width: 10 * scale, height: 10 * scale)
        layer.fill(
            Path(ellipseIn: avatar), with: .color(VignettePalette.avatar[(tint ?? design) % 3].opacity(0.85)))
        let x = avatar.maxX + 6 * scale
        bar(
            &layer, CGRect(x: x, y: rect.minY + 9.5 * scale, width: d.title * scale, height: 3.5 * scale),
            VignettePalette.ink.opacity(0.8))
        bar(
            &layer, CGRect(x: x, y: rect.minY + 17 * scale, width: d.sub * scale, height: 3 * scale),
            VignettePalette.dim)
    }

    /// A keycap, top-left at `origin`.
    static func keycap(
        _ ctx: inout GraphicsContext, _ label: String, origin: CGPoint, width: CGFloat = 22, pressed: Double
    ) {
        let rect = CGRect(x: origin.x, y: origin.y + CGFloat(pressed), width: width, height: 22)
        let cap = Path(roundedRect: rect, cornerRadius: 5, style: .continuous)
        var layer = ctx
        if pressed > 0 {
            layer.addFilter(.shadow(color: VignettePalette.phosphor.opacity(0.75 * pressed), radius: 6))
        }
        layer.fill(cap, with: .color(VignettePalette.cap))
        layer.fill(cap, with: .color(VignettePalette.phosphor.opacity(0.95 * pressed)))
        ctx.stroke(cap, with: .color(VignettePalette.edge.opacity(1 - 0.6 * pressed)), lineWidth: 0.6)
        let text = Text(label)
            .font(.system(size: 10, weight: .medium, design: .monospaced))
            .foregroundStyle(pressed > 0.5 ? Color.white : VignettePalette.ink.opacity(0.85))
        ctx.draw(text, at: CGPoint(x: rect.midX, y: rect.midY), anchor: .center)
    }

    /// A check centred on `center`, drawn on by `progress`, glowing.
    static func check(
        _ ctx: inout GraphicsContext, center: CGPoint, size: CGFloat = 5, progress: Double, opacity: Double = 1
    ) {
        guard progress > 0, opacity > 0 else { return }
        var path = Path()
        path.move(to: CGPoint(x: center.x - size, y: center.y))
        path.addLine(to: CGPoint(x: center.x - size * 0.3, y: center.y + size * 0.7))
        path.addLine(to: CGPoint(x: center.x + size, y: center.y - size * 0.8))
        var layer = ctx
        layer.opacity *= opacity
        layer.addFilter(.shadow(color: VignettePalette.phosphor.opacity(0.8), radius: 4))
        layer.stroke(
            path.trimmedPath(from: 0, to: progress),
            with: .color(VignettePalette.phosphorHot),
            style: StrokeStyle(lineWidth: size * 0.32, lineCap: .round, lineJoin: .round))
    }

    /// A ring round `center`, drawn clockwise from twelve by `progress`.
    static func ring(
        _ ctx: inout GraphicsContext, center: CGPoint, radius: CGFloat, progress: Double, color: Color,
        lineWidth: CGFloat = 1.5, opacity: Double = 1
    ) {
        guard progress > 0, opacity > 0 else { return }
        var path = Path()
        path.addArc(
            center: center, radius: radius, startAngle: .degrees(-90),
            endAngle: .degrees(-90 + 360 * min(1, progress)), clockwise: false)
        var layer = ctx
        layer.opacity *= opacity
        layer.stroke(path, with: .color(color), style: StrokeStyle(lineWidth: lineWidth, lineCap: .round))
    }

    /// Depth poses for a deck: offset down, scale and opacity for the card in
    /// front (0) and the three behind it. Depth 3 is invisible: where a new
    /// card waits.
    static let poseY: [CGFloat] = [0, 7, 13, 18]
    static let poseScale: [CGFloat] = [1, 0.93, 0.86, 0.8]
    static let poseOpacity: [Double] = [1, 0.55, 0.26, 0]

    /// The cards BEHIND the front one, each `promote` (0...1) of the way to the
    /// pose in front of it. `first` is how many cards have already left.
    static func deckBehind(
        _ ctx: inout GraphicsContext, origin: CGPoint, first: Int, promote: Double, tint: Int? = nil
    ) {
        // Back to front, so the nearest is drawn last.
        for depth in stride(from: 3, through: 1, by: -1) {
            let d = Double(depth) - promote
            let lo = Int(d.rounded(.down)), hi = min(3, lo + 1)
            let f = d - Double(lo)
            mail(
                &ctx,
                origin: CGPoint(x: origin.x, y: origin.y + VignetteClock.mix(poseY[lo], poseY[hi], f)),
                design: (first + depth) % 3, tint: tint,
                opacity: VignetteClock.mix(poseOpacity[lo], poseOpacity[hi], f),
                scale: VignetteClock.mix(poseScale[lo], poseScale[hi], f))
        }
    }
}
