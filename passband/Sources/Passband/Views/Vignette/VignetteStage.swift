// THE VIGNETTE STAGE: a small looping picture that DEMONSTRATES a gesture. Not
// a screenshot and not a video: an abstract little night screen where mail
// moves the way the keys move it. The What's New walk plays one per headliner.
//
// THE LANGUAGE (every scene is held to this):
//
//   CANVAS. 240 x 120 design units (2:1). Draw in those units only; the stage
//   scales the context to whatever size it is given. Sized for where it is
//   SEEN: across the top of the 580pt card a unit is about 2.4pt, so a 3-unit
//   bar sits near the height of a line of real text and the picture reads as
//   a product shot at nearly life size. (The discovery ring's 168 x 96 was
//   drawn for a 178pt tile; blown up to a card it made 76pt keycaps and bars
//   like piano keys.) Keep 10 units clear at every edge.
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
//   an avatar dot, a title bar and a sub bar; in a list it is a row of the
//   same parts. Bars, never letters: no text, no digits, no real names
//   anywhere in a scene. The one exception is a keycap's label, because the
//   key IS the lesson.
//
//   KEYCAPS. `VignetteKit.keycap`: 18 units tall, even border. Pressed, the
//   cap fills `phosphor`, its label goes white, it sinks 1 unit and glows
//   evenly all round. Label a key exactly as the app's own hint does (`u`,
//   not `U`: the app's `Kbd` is lower case, and so is the card's line).
//
//   LOOP. 4.8 seconds, built from beats: rest, gesture, rest. A loop BEGINS
//   AND ENDS ON THE SAME FRAME, with stillness at the seam, so the repeat is
//   invisible. Never an empty screen mid-loop: the next thing arrives while
//   the last one leaves. Write the scene as a PURE function of `t` in
//   0..<loop.
//
//   MOTION. Translate, fade, a scale of at most 0.93...1.05, a stroke drawing
//   on. No bounce past 1, no rotation, no blur, no particles. One ACTION at a
//   time: a key's consequences may move two things together (a page and its
//   row), but never two gestures at once.
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
    static let canvas = CGSize(width: 240, height: 120)
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
    /// The design units the scene draws in. The card's 240 x 120 unless the
    /// scene says otherwise: the iPhone's walk gives a picture most of a
    /// portrait page, and its scenes are drawn square for it
    /// (PassbandiOS/Views/PhoneVignettes.swift). Everything else in the
    /// language above holds at any canvas.
    static var canvas: CGSize { get }
    /// How long one pass takes. 4.8 seconds unless the scene says otherwise:
    /// a thumb's gesture has more parts than a key's (down, drag, lift, tap),
    /// and a phone scene that needs them all may take a little longer.
    static var loop: Double { get }
    static func draw(_ ctx: inout GraphicsContext, t: Double)
}

extension VignetteDrawing {
    static var canvas: CGSize { VignetteClock.canvas }
    static var loop: Double { VignetteClock.loop }
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
    /// False lets the stage take whatever shape it is given instead of its
    /// canvas's own: the ground fills it and the scene is fitted and centred,
    /// so a page shorter than the picture wants shrinks the scene and keeps
    /// the screen.
    var fitsCanvas = true
    /// How much of the stage the SCENE keeps clear of, in points. The ground
    /// still fills all of it: the phone's walk bleeds the night to the top of
    /// the screen and sets its wordmark on it, and the picture has to start
    /// below that.
    var sceneInsets = EdgeInsets()

    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var onScreen = false
    /// When this stage started playing, so every page of a walk opens on its
    /// own first beat instead of somewhere in the middle of a loop that has
    /// been running against the wall clock all along.
    @State private var startedAt = Date()

    private var running: Bool { live && onScreen && !reduceMotion && fixedTime == nil }

    var body: some View {
        // Paused, the timeline draws once and stops asking for frames.
        TimelineView(.animation(minimumInterval: 1 / 60, paused: !running)) { timeline in
            let t = time(timeline.date)
            Canvas { ctx, size in
                Self.render(&ctx, size: size, scene: scene, t: t, insets: sceneInsets)
            }
        }
        .modifier(CanvasShape(ratio: fitsCanvas ? scene.canvas.width / scene.canvas.height : nil))
        .clipShape(RoundedRectangle(cornerRadius: cornerRadius, style: .continuous))
        .overlay {
            if cornerRadius > 0 {
                RoundedRectangle(cornerRadius: cornerRadius, style: .continuous)
                    .strokeBorder(VignettePalette.edge, lineWidth: 0.5)
            }
        }
        .onAppear {
            onScreen = true
            startedAt = .now
        }
        .onDisappear { onScreen = false }
        .onChange(of: running) { _, now in
            // Coming back from the poster (the window became key again)
            // starts the loop over rather than cutting into it.
            if now { startedAt = .now }
        }
        .accessibilityHidden(true)
    }

    private func time(_ date: Date) -> Double {
        if let fixedTime { return fixedTime }
        guard running else { return scene.posterTime }
        return max(0, date.timeIntervalSince(startedAt)).truncatingRemainder(dividingBy: scene.loop)
    }

    /// One frame, ground included. Static so a frame can be drawn with no view
    /// around it. The ground fills whatever it is given; the scene is fitted
    /// to the height and centred, so a band wider than 2:1 is more night
    /// either side rather than a stretched picture.
    static func render(
        _ ctx: inout GraphicsContext, size: CGSize, scene: any VignetteDrawing.Type, t: Double,
        insets: EdgeInsets = EdgeInsets()
    ) {
        let full = CGRect(origin: .zero, size: size)
        ctx.fill(
            Path(full),
            with: .linearGradient(
                Gradient(colors: [VignettePalette.groundTop, VignettePalette.groundBottom]),
                startPoint: .zero, endPoint: CGPoint(x: 0, y: size.height)))
        // A wide, low phosphor bloom from below centre: the brand's light,
        // still, so the screen is lit rather than black.
        ctx.fill(
            Path(full),
            with: .radialGradient(
                Gradient(colors: [VignettePalette.phosphor.opacity(0.15), .clear]),
                center: CGPoint(x: full.midX, y: full.maxY + size.height * 0.1),
                startRadius: 0, endRadius: size.width * 0.6))
        let canvas = scene.canvas
        let room = CGRect(
            x: insets.leading, y: insets.top,
            width: max(1, size.width - insets.leading - insets.trailing),
            height: max(1, size.height - insets.top - insets.bottom))
        let scale = min(room.width / canvas.width, room.height / canvas.height)
        ctx.translateBy(
            x: room.minX + (room.width - canvas.width * scale) / 2,
            y: room.minY + (room.height - canvas.height * scale) / 2)
        ctx.scaleBy(x: scale, y: scale)
        scene.draw(&ctx, t: t)
    }
}

/// The canvas's own shape, or none at all. Not `aspectRatio(nil)`: that is not
/// "no ratio", it is the content's IDEAL ratio, and a Canvas's is square.
private struct CanvasShape: ViewModifier {
    let ratio: CGFloat?

    func body(content: Content) -> some View {
        if let ratio {
            content.aspectRatio(ratio, contentMode: .fit)
        } else {
            content
        }
    }
}

// MARK: - shared parts

enum VignetteKit {
    /// A rounded bar: the stand-in for a line of text.
    static func bar(_ ctx: inout GraphicsContext, _ rect: CGRect, _ color: Color) {
        ctx.fill(Path(roundedRect: rect, cornerRadius: rect.height / 2), with: .color(color))
    }

    /// An avatar dot centred on `center`.
    static func avatar(_ ctx: inout GraphicsContext, center: CGPoint, diameter: CGFloat, tint: Int, opacity: Double = 0.9) {
        ctx.fill(
            Path(ellipseIn: CGRect(
                x: center.x - diameter / 2, y: center.y - diameter / 2, width: diameter, height: diameter)),
            with: .color(VignettePalette.avatar[tint % 3].opacity(opacity)))
    }

    /// One email card, top-left at `origin`. `design` (0..<3) picks the bar
    /// widths; `tint` picks the sender and defaults to the design's own, so a
    /// deck of three designs is three senders and a stack that passes one tint
    /// is one sender's mail. Opacities multiply the layer's own, so a part
    /// drawn into a fading layer fades with it.
    static func mail(
        _ ctx: inout GraphicsContext, origin: CGPoint, width: CGFloat = 112, design: Int,
        tint: Int? = nil, opacity: Double, scale: CGFloat = 1, lit: Double = 0, blank: Bool = false
    ) {
        let designs: [(title: CGFloat, sub: CGFloat)] = [(44, 66), (34, 58), (52, 42)]
        let d = designs[design % designs.count]
        let w = width * scale
        let h: CGFloat = 30 * scale
        let rect = CGRect(x: origin.x + (width - w) / 2, y: origin.y, width: w, height: h)
        var layer = ctx
        layer.opacity *= opacity
        let card = Path(roundedRect: rect, cornerRadius: 6 * scale, style: .continuous)
        layer.fill(card, with: .color(VignettePalette.card))
        layer.fill(card, with: .color(VignettePalette.faint))
        layer.stroke(card, with: .color(VignettePalette.edge), lineWidth: 0.5)
        if lit > 0 {
            layer.stroke(card, with: .color(VignettePalette.phosphor.opacity(0.85 * lit)), lineWidth: 0.7)
        }
        // A card deep in a fanned stack is an edge, not a second email to read.
        guard !blank else { return }
        avatar(&layer, center: CGPoint(x: rect.minX + 13 * scale, y: rect.midY), diameter: 10 * scale, tint: tint ?? design)
        let x = rect.minX + 24 * scale
        bar(
            &layer, CGRect(x: x, y: rect.minY + 10 * scale, width: d.title * scale, height: 3 * scale),
            VignettePalette.ink.opacity(0.8))
        bar(
            &layer, CGRect(x: x, y: rect.minY + 17 * scale, width: d.sub * scale, height: 2.5 * scale),
            VignettePalette.dim)
    }

    /// A keycap, top-left at `origin`.
    static func keycap(
        _ ctx: inout GraphicsContext, _ label: String, origin: CGPoint, width: CGFloat = 18, pressed: Double
    ) {
        let rect = CGRect(x: origin.x, y: origin.y + CGFloat(pressed), width: width, height: 18)
        let cap = Path(roundedRect: rect, cornerRadius: 4, style: .continuous)
        var layer = ctx
        if pressed > 0 {
            layer.addFilter(.shadow(color: VignettePalette.phosphor.opacity(0.75 * pressed), radius: 5))
        }
        layer.fill(cap, with: .color(VignettePalette.cap))
        layer.fill(cap, with: .color(VignettePalette.phosphor.opacity(0.95 * pressed)))
        ctx.stroke(cap, with: .color(Color.white.opacity(0.14 * (1 - 0.6 * pressed))), lineWidth: 0.5)
        let text = Text(label)
            .font(.system(size: 8.5, weight: .medium, design: .monospaced))
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
        layer.addFilter(.shadow(color: VignettePalette.phosphor.opacity(0.8), radius: 3))
        layer.stroke(
            path.trimmedPath(from: 0, to: progress),
            with: .color(VignettePalette.phosphorHot),
            style: StrokeStyle(lineWidth: max(1.1, size * 0.3), lineCap: .round, lineJoin: .round))
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

    /// An arc of `sweep` (0...1 of a turn) whose head is at `head` (turns from
    /// twelve). A spinner made of position, not rotation: the arc's ends move
    /// round the circle, nothing is turned.
    static func arc(
        _ ctx: inout GraphicsContext, center: CGPoint, radius: CGFloat, head: Double, sweep: Double,
        color: Color, lineWidth: CGFloat = 1.2, opacity: Double = 1
    ) {
        guard sweep > 0, opacity > 0 else { return }
        var path = Path()
        path.addArc(
            center: center, radius: radius, startAngle: .degrees(-90 + 360 * (head - sweep)),
            endAngle: .degrees(-90 + 360 * head), clockwise: false)
        var layer = ctx
        layer.opacity *= opacity
        layer.stroke(path, with: .color(color), style: StrokeStyle(lineWidth: lineWidth, lineCap: .round))
    }

    /// Depth poses for a deck: offset down, scale and opacity for the card in
    /// front (0) and the three behind it. Depth 3 is invisible: where a new
    /// card waits.
    static let poseY: [CGFloat] = [0, 6, 11, 15]
    static let poseScale: [CGFloat] = [1, 0.94, 0.88, 0.82]
    static let poseOpacity: [Double] = [1, 0.55, 0.26, 0]

    /// The cards BEHIND the front one, each `promote` (0...1) of the way to the
    /// pose in front of it. `first` is how many cards have already left.
    static func deckBehind(
        _ ctx: inout GraphicsContext, origin: CGPoint, width: CGFloat = 112, first: Int, promote: Double,
        tint: Int? = nil
    ) {
        // Back to front, so the nearest is drawn last.
        for depth in stride(from: 3, through: 1, by: -1) {
            let d = Double(depth) - promote
            let lo = Int(d.rounded(.down)), hi = min(3, lo + 1)
            let f = d - Double(lo)
            mail(
                &ctx,
                origin: CGPoint(x: origin.x, y: origin.y + VignetteClock.mix(poseY[lo], poseY[hi], f)),
                width: width, design: (first + depth) % 3, tint: tint,
                opacity: VignetteClock.mix(poseOpacity[lo], poseOpacity[hi], f),
                scale: VignetteClock.mix(poseScale[lo], poseScale[hi], f))
        }
    }
}
