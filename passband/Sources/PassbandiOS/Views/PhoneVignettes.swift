// THE SCENES the phone's What's New walk plays, one per `PhoneVignette` key in
// the phone's release table (Lib/PhoneReleaseNotes.swift). They are held to the
// language at the top of Views/Vignette/VignetteStage.swift and run on its
// stage: a pure function of time on a night screen, bars for text, one actor
// colour, a loop that ends on the frame it began on.
//
// WHAT A PHONE CHANGES ABOUT THAT LANGUAGE, and nothing else does:
//
//   CANVAS. 220 x 200, near square, because the walk gives the picture most of
//   a portrait page where the Mac's card gives it a 2:1 strip. Across a phone a
//   unit is about 1.8pt, so a message card stands at roughly two thirds of its
//   real size.
//
//   THE THUMB, where the Mac's scenes have a keycap. A phone has no keys, so
//   the lesson is where the thumb goes and what it does there: a soft ring
//   that lands, presses, drags and lifts (`PhoneKit.touch`). One thumb, and it
//   is gone whenever it is not doing something.
//
//   GLYPHS ON CONTROLS. The Mac allows letters on a keycap because the key IS
//   the lesson. Here the control is: a disc or a rail button carries the glyph
//   the app's own carries, and still no word.
//
//   ONE MORE COLOUR, in one scene. Remind is amber in the app (`Palette.warn`
//   on the dashboard's rail), and a rail drawn without it would not be
//   recognised as that rail. It is the control's own colour, not a tier.
//
// The real screens these are drawn from: Views/ThreadViewer.swift and
// PassbandiOS/Views/ChainFoot.swift (the chain's end and a card's rail),
// Views/InlineReply.swift (the composer), PassbandiOS/Views/SwipeRow.swift
// (the dashboard's rail), and the checkmark's hold in ThreadViewer.
//
// ADDING ONE: a case on `PhoneVignette`, a scene here, and its line in `scene`
// below. The compiler insists on the last.

import SwiftUI

extension PhoneVignette {
    var scene: any VignetteDrawing.Type {
        switch self {
        case .chainReply: ChainReplyScene.self
        case .messageSwipe: MessageSwipeScene.self
        case .clearStack: ClearStackPhoneScene.self
        case .remind: RemindScene.self
        }
    }
}

// MARK: - shared parts

enum PhoneKit {
    static let canvas = CGSize(width: 220, height: 200)

    /// The reader's column: one message card's left edge and width.
    static let columnX: CGFloat = 14
    static let columnWidth: CGFloat = 192

    /// The rail's second and third verbs. The first is `phosphor`.
    static let replyAll = Color(hex: 0x3A7CC4)
    static let neutral = Color(hex: 0x7C8898)
    /// Remind, as the dashboard's rail paints it.
    static let remind = Color(hex: 0xE8AC4C)

    /// The thumb: a soft ring where it touches, a little smaller and brighter
    /// while it presses. Lit evenly all round.
    static func touch(_ ctx: inout GraphicsContext, at point: CGPoint, presence: Double, down: Double = 1) {
        guard presence > 0.01 else { return }
        let radius = VignetteClock.mix(9, 7.5, down)
        let ring = Path(ellipseIn: CGRect(
            x: point.x - radius, y: point.y - radius, width: radius * 2, height: radius * 2))
        var layer = ctx
        layer.opacity *= presence
        layer.fill(ring, with: .color(Color.white.opacity(VignetteClock.mix(0.12, 0.3, down))))
        layer.stroke(ring, with: .color(Color.white.opacity(VignetteClock.mix(0.4, 0.75, down))), lineWidth: 0.7)
    }

    /// One SF Symbol centred on `center`: a control's own mark.
    static func glyph(
        _ ctx: inout GraphicsContext, _ name: String, at center: CGPoint, size: CGFloat, color: Color,
        weight: Font.Weight = .semibold
    ) {
        ctx.draw(
            Text(Image(systemName: name)).font(.system(size: size, weight: weight)).foregroundStyle(color),
            at: center, anchor: .center)
    }

    /// A rounded surface of the reader's own make: card fill, even edge, and a
    /// phosphor edge over it by `lit`.
    static func surface(
        _ ctx: inout GraphicsContext, _ rect: CGRect, radius: CGFloat = 7, lit: Double = 0,
        fill: Color = VignettePalette.card
    ) {
        let shape = Path(roundedRect: rect, cornerRadius: radius, style: .continuous)
        ctx.fill(shape, with: .color(fill))
        ctx.fill(shape, with: .color(VignettePalette.faint))
        ctx.stroke(shape, with: .color(VignettePalette.edge), lineWidth: 0.5)
        if lit > 0 {
            ctx.stroke(shape, with: .color(VignettePalette.phosphor.opacity(0.85 * lit)), lineWidth: 0.7)
        }
    }

    /// One message of a thread: who, when, and a few lines of what they said.
    /// `design` picks the line widths, `tint` the sender.
    static func message(
        _ ctx: inout GraphicsContext, _ rect: CGRect, tint: Int, design: Int, opacity: Double = 1,
        lit: Double = 0, sent: Double = 0
    ) {
        guard opacity > 0.01 else { return }
        let bodies: [[CGFloat]] = [[150, 164, 96], [160, 132, 148], [140, 158, 70]]
        let names: [CGFloat] = [34, 26, 40]
        var layer = ctx
        layer.opacity *= opacity
        surface(&layer, rect, lit: lit)
        VignetteKit.avatar(&layer, center: CGPoint(x: rect.minX + 13, y: rect.minY + 12), diameter: 11, tint: tint)
        VignetteKit.bar(
            &layer, CGRect(x: rect.minX + 23, y: rect.minY + 10.5, width: names[design % 3], height: 3),
            VignettePalette.ink.opacity(0.85))
        // The date, on the trailing edge; a sent reply wears its check there.
        VignetteKit.bar(
            &layer, CGRect(x: rect.maxX - 30, y: rect.minY + 10.8, width: 20, height: 2.4),
            VignettePalette.dim.opacity(1 - sent))
        VignetteKit.check(&layer, center: CGPoint(x: rect.maxX - 16, y: rect.minY + 12), size: 3.4, progress: sent)
        let lines = max(1, Int((rect.height - 26) / 7.5))
        for (index, width) in bodies[design % 3].prefix(lines).enumerated() {
            VignetteKit.bar(
                &layer, CGRect(x: rect.minX + 10, y: rect.minY + 24 + CGFloat(index) * 7.5, width: width, height: 2.6),
                VignettePalette.dim.opacity(0.9))
        }
    }

    /// The end of a chain: reply and reply all, two glass capsules of equal
    /// weight. `pressed` lights the first.
    static func doors(_ ctx: inout GraphicsContext, y: CGFloat, opacity: Double, pressed: Double = 0) {
        guard opacity > 0.01 else { return }
        var layer = ctx
        layer.opacity *= opacity
        let width = (columnWidth - 6) / 2
        for index in 0..<2 {
            let rect = CGRect(x: columnX + CGFloat(index) * (width + 6), y: y, width: width, height: 18)
            let shape = Path(roundedRect: rect, cornerRadius: 9)
            let down = index == 0 ? pressed : 0
            var door = layer
            if down > 0 {
                door.addFilter(.shadow(color: VignettePalette.phosphor.opacity(0.7 * down), radius: 4))
            }
            door.fill(shape, with: .color(VignettePalette.cap))
            door.fill(shape, with: .color(VignettePalette.phosphor.opacity(0.28 * down)))
            layer.stroke(
                shape, with: .color(VignettePalette.phosphor.opacity(VignetteClock.mix(0.28, 0.9, down))), lineWidth: 0.5)
            let label: CGFloat = index == 0 ? 16 : 26
            let start = rect.midX - (label + 11) / 2
            glyph(
                &layer, index == 0 ? "arrowshape.turn.up.left" : "arrowshape.turn.up.left.2",
                at: CGPoint(x: start + 4, y: rect.midY), size: 6, color: VignettePalette.phosphor)
            VignetteKit.bar(
                &layer, CGRect(x: start + 11, y: rect.midY - 1.3, width: label, height: 2.6),
                VignettePalette.ink.opacity(0.8))
        }
    }

    /// The reply composer as it sits under a chain: who it answers, the lines
    /// written so far, and send. `typed` is 0...1 through its three lines;
    /// `tint` is the avatar of the message being answered.
    static let composerHeight: CGFloat = 62

    static func composer(
        _ ctx: inout GraphicsContext, y: CGFloat, tint: Int, typed: Double, send: Double = 0, opacity: Double
    ) {
        guard opacity > 0.01 else { return }
        var layer = ctx
        layer.opacity *= opacity
        let rect = CGRect(x: columnX, y: y, width: columnWidth, height: composerHeight)
        let shape = Path(roundedRect: rect, cornerRadius: 9, style: .continuous)
        layer.fill(shape, with: .color(VignettePalette.card))
        layer.fill(shape, with: .color(VignettePalette.phosphor.opacity(0.07)))
        layer.stroke(shape, with: .color(VignettePalette.phosphor.opacity(0.5)), lineWidth: 0.6)

        // "replying to", and who.
        glyph(
            &layer, "arrowshape.turn.up.left.fill", at: CGPoint(x: rect.minX + 12, y: rect.minY + 9.5), size: 6,
            color: VignettePalette.phosphor)
        VignetteKit.bar(&layer, CGRect(x: rect.minX + 19, y: rect.minY + 8.3, width: 24, height: 2.4), VignettePalette.dim)
        VignetteKit.avatar(&layer, center: CGPoint(x: rect.minX + 50, y: rect.minY + 9.5), diameter: 6, tint: tint)
        VignetteKit.bar(
            &layer, CGRect(x: rect.minX + 56, y: rect.minY + 8.1, width: 28, height: 2.8),
            VignettePalette.ink.opacity(0.85))
        layer.fill(
            Path(CGRect(x: rect.minX, y: rect.minY + 18, width: rect.width, height: 0.5)),
            with: .color(VignettePalette.edge))

        // The words, a line at a time, the caret riding the end of the one
        // being written.
        let widths: [CGFloat] = [128, 156, 74]
        var caret = CGPoint(x: rect.minX + 10, y: rect.minY + 24)
        for (index, width) in widths.enumerated() {
            let p = min(1, max(0, typed * 3 - Double(index)))
            guard p > 0 else { continue }
            let lineY = rect.minY + 25 + CGFloat(index) * 7
            VignetteKit.bar(
                &layer, CGRect(x: rect.minX + 10, y: lineY, width: max(2.6, width * CGFloat(p)), height: 2.6),
                VignettePalette.ink.opacity(0.82))
            caret = CGPoint(x: rect.minX + 12 + width * CGFloat(p), y: lineY - 1)
        }
        layer.fill(
            Path(roundedRect: CGRect(x: caret.x, y: caret.y - 1.2, width: 1.1, height: 7), cornerRadius: 0.5),
            with: .color(VignettePalette.phosphorHot))

        // Its foot: the two chips, and send.
        for (x, width) in [(CGFloat(10), CGFloat(26)), (40, 36)] {
            let chip = Path(roundedRect: CGRect(x: rect.minX + x, y: rect.maxY - 15, width: width, height: 9), cornerRadius: 4.5)
            layer.stroke(chip, with: .color(VignettePalette.edge), lineWidth: 0.5)
            VignetteKit.bar(
                &layer, CGRect(x: rect.minX + x + 5, y: rect.maxY - 11.6, width: width - 10, height: 2.2),
                VignettePalette.dim)
        }
        let pill = CGRect(x: rect.maxX - 44, y: rect.maxY - 16.5 + CGFloat(send), width: 34, height: 12)
        var glow = layer
        if send > 0 {
            glow.addFilter(.shadow(color: VignettePalette.phosphor.opacity(0.8 * send), radius: 5))
        }
        glow.fill(Path(roundedRect: pill, cornerRadius: 6), with: .color(VignettePalette.phosphor))
        VignetteKit.bar(
            &layer, CGRect(x: pill.midX - 8, y: pill.midY - 1.3, width: 16, height: 2.6), Color.white.opacity(0.95))
    }

    static func sendPoint(composerY y: CGFloat) -> CGPoint {
        CGPoint(x: columnX + columnWidth - 27, y: y + composerHeight - 10.5)
    }
}

// MARK: - reply at the end of the chain

/// REPLY WHERE YOU FINISH READING: a thread, and under its last message the
/// two capsules every email now ends in. The thumb presses reply; the composer
/// takes their place, in the chain, under the mail it answers. Three lines are
/// written, send goes down, and the composer becomes what it was writing: one
/// more message in the thread, with its check.
///
/// Then the thread is finished with and the next one comes in from the
/// trailing side, which is the frame the loop began on: two messages, the two
/// capsules, a thumb nowhere.
enum ChainReplyScene: VignetteDrawing {
    static let canvas = PhoneKit.canvas
    static let loop: Double = 5.6

    private static let tapDoor: Double = 0.7
    private static let typeFrom: Double = 1.35
    private static let typeTo: Double = 2.55
    private static let tapSend: Double = 2.95
    private static let slideFrom: Double = 4.2
    private static let slideTo: Double = 4.85

    /// The composer open under the chain with its words in it.
    static let posterTime: Double = 2.7

    private static let cardY: [CGFloat] = [14, 70]
    private static let footY: CGFloat = 126

    static func draw(_ ctx: inout GraphicsContext, t: Double) {
        let ease = VignetteClock.ease
        // The thread on screen leaves to the leading side as the next arrives
        // from the trailing one.
        let slide = ease(t, slideFrom, slideTo)
        // One goes before the other comes: two threads at half strength on
        // top of each other is neither of them.
        let gone = ease(slide, 0, 0.6), come = ease(slide, 0.4, 1)
        if gone < 1 {
            var leaving = ctx
            leaving.opacity *= 1 - gone
            leaving.translateBy(x: -60 * CGFloat(slide), y: 0)
            drawThread(&leaving, t: t, fresh: false)
        }
        if come > 0 {
            var arriving = ctx
            arriving.opacity *= come
            arriving.translateBy(x: 60 * CGFloat(1 - slide), y: 0)
            drawThread(&arriving, t: 0, fresh: true)
        }

        // The thumb: once on reply, once on send.
        let onDoor = ease(t, tapDoor - 0.34, tapDoor - 0.18) * (1 - ease(t, tapDoor + 0.22, tapDoor + 0.4))
        PhoneKit.touch(
            &ctx, at: CGPoint(x: PhoneKit.columnX + 46, y: footY + 12), presence: onDoor,
            down: VignetteClock.press(t, at: tapDoor))
        let onSend = ease(t, tapSend - 0.34, tapSend - 0.18) * (1 - ease(t, tapSend + 0.22, tapSend + 0.4))
        PhoneKit.touch(
            &ctx, at: PhoneKit.sendPoint(composerY: footY), presence: onSend,
            down: VignetteClock.press(t, at: tapSend))
    }

    /// `fresh` is the next thread arriving: at rest, whatever `t` says.
    private static func drawThread(_ ctx: inout GraphicsContext, t: Double, fresh: Bool) {
        let ease = VignetteClock.ease
        PhoneKit.message(&ctx, CGRect(x: PhoneKit.columnX, y: cardY[0], width: PhoneKit.columnWidth, height: 50), tint: 2, design: 0)
        PhoneKit.message(&ctx, CGRect(x: PhoneKit.columnX, y: cardY[1], width: PhoneKit.columnWidth, height: 50), tint: 1, design: 1)

        let composing = fresh ? 0 : ease(t, tapDoor + 0.1, tapDoor + 0.42)
        let sent = fresh ? 0 : ease(t, tapSend + 0.14, tapSend + 0.5)
        PhoneKit.doors(
            &ctx, y: footY + 2, opacity: 1 - (fresh ? 0 : ease(t, tapDoor + 0.06, tapDoor + 0.24)),
            pressed: fresh ? 0 : VignetteClock.press(t, at: tapDoor))

        // The composer rises a little into place, and becomes the message it
        // wrote: the same card, the same place, its chrome gone.
        var writing = ctx
        writing.translateBy(x: 0, y: 6 * CGFloat(1 - composing))
        PhoneKit.composer(
            &writing, y: footY, tint: 1, typed: VignetteClock.ramp(t, typeFrom, typeTo),
            send: fresh ? 0 : VignetteClock.press(t, at: tapSend), opacity: composing * (1 - sent))
        PhoneKit.message(
            &ctx, CGRect(x: PhoneKit.columnX, y: footY, width: PhoneKit.columnWidth, height: 50), tint: 0,
            design: 2, opacity: sent, lit: sent * (1 - ease(t, tapSend + 0.8, tapSend + 1.15)),
            sent: fresh ? 0 : ease(t, tapSend + 0.42, tapSend + 0.62))
    }
}

// MARK: - a message's rail

/// ANSWER ONE MESSAGE: the thumb lands on a message and drags it to the left,
/// and the three verbs about THAT message are standing where it was: reply,
/// reply all, forward, each a disc with its name under it. The thumb lifts,
/// comes back down on reply, and the rail shuts. The composer arrives at the
/// end of the chain, where it always is, naming the sender of the message
/// that was swiped, which stays lit for as long as it is being answered.
///
/// The composer is dismissed and the two capsules are back: the frame the
/// loop began on.
enum MessageSwipeScene: VignetteDrawing {
    static let canvas = PhoneKit.canvas

    private static let dragFrom: Double = 0.55
    private static let dragTo: Double = 1.05
    private static let tap: Double = 2.0
    private static let closeFrom: Double = 2.14
    private static let leaveFrom: Double = 3.75

    /// The rail out, the thumb still on the message that pulled it.
    static let posterTime: Double = 1.12

    private static let card = CGRect(x: PhoneKit.columnX, y: 66, width: PhoneKit.columnWidth, height: 54)
    private static let pull: CGFloat = 94
    private static let footY: CGFloat = 126

    private static let verbs: [(symbol: String, color: Color, label: CGFloat)] = [
        // Reply all leads, as on the real rail: it is what a full swipe sends.
        ("arrowshape.turn.up.left.2.fill", VignettePalette.phosphor, 20),
        ("arrowshape.turn.up.left.fill", PhoneKit.replyAll, 14),
        ("arrowshape.turn.up.right.fill", PhoneKit.neutral, 18),
    ]

    private static func disc(_ index: Int) -> CGPoint {
        CGPoint(x: card.maxX - pull + 18 + CGFloat(index) * 30, y: card.midY - 4)
    }

    static func draw(_ ctx: inout GraphicsContext, t: Double) {
        let ease = VignetteClock.ease
        let out = ease(t, dragFrom, dragTo) - ease(t, closeFrom, closeFrom + 0.36)
        let answering = ease(t, closeFrom + 0.1, closeFrom + 0.46) - ease(t, leaveFrom, leaveFrom + 0.36)
        let pressed = VignetteClock.press(t, at: tap)

        // The message before it: there, and not what this is about.
        PhoneKit.message(
            &ctx, CGRect(x: PhoneKit.columnX, y: 14, width: PhoneKit.columnWidth, height: 46), tint: 2, design: 0,
            opacity: 0.7)

        // The rail, standing still while the message moves off it. Each disc
        // comes up as the edge passes it, nearest the edge first.
        for (index, verb) in verbs.enumerated() {
            let lead = Double(verbs.count - 1 - index) * 0.16
            let shown = min(1, max(0, (out - lead) / (1 - lead - 0.2)))
            guard shown > 0.01 else { continue }
            let center = disc(index)
            let radius = 10.5 * VignetteClock.mix(0.8, 1, shown)
            let down = index == 0 ? pressed : 0
            var layer = ctx
            layer.opacity *= shown
            var glow = layer
            if down > 0 {
                glow.addFilter(.shadow(color: VignettePalette.phosphor.opacity(0.85 * down), radius: 5))
            }
            glow.fill(
                Path(ellipseIn: CGRect(x: center.x - radius, y: center.y - radius, width: radius * 2, height: radius * 2)),
                with: .color(verb.color))
            PhoneKit.glyph(&layer, verb.symbol, at: center, size: 8.5, color: .white)
            VignetteKit.bar(
                &layer, CGRect(x: center.x - verb.label / 2, y: center.y + 15, width: verb.label, height: 2.2),
                VignettePalette.dim)
        }

        // The message itself, off the leading edge of the screen as it goes.
        PhoneKit.message(
            &ctx, card.offsetBy(dx: -pull * CGFloat(out), dy: 0), tint: 1, design: 1,
            lit: max(out * 0.5, answering))

        // The chain's end: the capsules, and the composer that takes their
        // place when the disc is pressed.
        PhoneKit.doors(&ctx, y: footY + 2, opacity: 0.75 * (1 - answering))
        var writing = ctx
        writing.translateBy(x: 0, y: 6 * CGFloat(1 - answering))
        PhoneKit.composer(&writing, y: footY, tint: 1, typed: 0, opacity: answering)

        // The thumb: down on the message and across, then once on reply.
        let dragging = ease(t, dragFrom - 0.25, dragFrom - 0.1) * (1 - ease(t, dragTo + 0.16, dragTo + 0.34))
        PhoneKit.touch(
            &ctx, at: CGPoint(x: VignetteClock.mix(178, 178 - pull, ease(t, dragFrom, dragTo)), y: card.midY + 6),
            presence: dragging, down: VignetteClock.hold(t, from: dragFrom - 0.08, to: dragTo + 0.14))
        let tapping = ease(t, tap - 0.34, tap - 0.18) * (1 - ease(t, tap + 0.2, tap + 0.36))
        PhoneKit.touch(&ctx, at: disc(0), presence: tapping, down: pressed)
    }
}

// MARK: - hold the checkmark

/// CLEAR A STACK: the reader on one sender's pile of Reading mail, its
/// checkmark in the top corner where the reader keeps it. The thumb goes down
/// on the checkmark and stays down. A ring closes in the middle of the screen
/// at the app's own pace, and when it closes every email in the pile takes its
/// check, top to bottom, and the pile goes. The next sender's rises into its
/// place and the thumb goes down again.
///
/// The Mac's scene of the same feature (`ClearStackScene`) with a thumb for
/// its key, and the same two holds a loop for the same reason: one clear takes
/// two seconds, and a loop that cleared once and sat still read as a stall.
enum ClearStackPhoneScene: VignetteDrawing {
    static let canvas = PhoneKit.canvas

    private static let cycle: Double = VignetteClock.loop / 2
    /// Within a cycle. The 180ms before the ring and its 750ms are the app's.
    private static let down: Double = 0.4
    private static let ringFrom: Double = down + 0.18
    private static let ringTo: Double = ringFrom + 0.75
    private static let up: Double = ringTo + 0.12
    private static let leaveFrom: Double = ringTo + 0.4
    private static let returnFrom: Double = leaveFrom + 0.14

    /// Held, the ring a little past half.
    static let posterTime: Double = ringFrom + 0.45

    private static let pile = CGRect(x: 22, y: 50, width: 176, height: 104)
    private static let checkmark = CGPoint(x: 194, y: 26)
    private static let hud = CGRect(x: 80, y: 71, width: 60, height: 62)
    private static let stacks: [(tint: Int, name: CGFloat, rows: [CGFloat])] = [
        (2, 44, [104, 84, 116, 72]),
        (1, 34, [92, 112, 78, 100]),
    ]

    static func draw(_ ctx: inout GraphicsContext, t: Double) {
        let ease = VignetteClock.ease
        let which = t < cycle ? 0 : 1
        let c = t - Double(which) * cycle
        let leaving = ease(c, leaveFrom, leaveFrom + 0.36)
        let back = ease(c, returnFrom, returnFrom + 0.44)
        let returning = c >= returnFrom
        let held = VignetteClock.hold(c, from: down, to: up)

        if !returning || leaving < 1 {
            drawPile(&ctx, stacks[which], c: c, dy: -16 * CGFloat(leaving), opacity: 1 - leaving, checks: true)
        }
        if returning {
            drawPile(&ctx, stacks[1 - which], c: c, dy: 12 * CGFloat(1 - back), opacity: back, checks: false)
        }

        // The reader's bar: back, and the checkmark this is about.
        chrome(&ctx, at: CGPoint(x: 26, y: checkmark.y), symbol: "chevron.left", color: VignettePalette.ink.opacity(0.7), pressed: 0)
        chrome(&ctx, at: checkmark, symbol: "checkmark", color: VignettePalette.phosphor, pressed: held)

        // The hold's own surface, at the centre: the track, the ring that
        // closes round it, the check, and a line saying what a full ring does.
        let closed = ease(c, ringTo - 0.04, ringTo + 0.04)
        let shown = ease(c, ringFrom - 0.1, ringFrom + 0.04) * (1 - ease(c, up + 0.02, up + 0.24))
        if shown > 0.01 {
            var layer = ctx
            layer.opacity *= shown
            let plate = Path(roundedRect: hud, cornerRadius: 13, style: .continuous)
            layer.fill(plate, with: .color(VignettePalette.cap.opacity(0.94)))
            layer.stroke(plate, with: .color(Color.white.opacity(0.14)), lineWidth: 0.5)
            let center = CGPoint(x: hud.midX, y: hud.minY + 26)
            VignetteKit.ring(&layer, center: center, radius: 17, progress: 1, color: Color.white.opacity(0.12), lineWidth: 1.6)
            var glow = layer
            if closed > 0 {
                glow.addFilter(.shadow(color: VignettePalette.phosphor.opacity(0.9 * closed), radius: 4))
            }
            VignetteKit.ring(
                &glow, center: center, radius: 17, progress: VignetteClock.ramp(c, ringFrom, ringTo),
                color: closed > 0.5 ? VignettePalette.phosphorHot : VignettePalette.phosphor, lineWidth: 1.8)
            var mark = Path()
            mark.move(to: CGPoint(x: center.x - 7, y: center.y + 0.5))
            mark.addLine(to: CGPoint(x: center.x - 2, y: center.y + 5.5))
            mark.addLine(to: CGPoint(x: center.x + 7.5, y: center.y - 6))
            layer.stroke(
                mark, with: .color(VignettePalette.ink.opacity(0.92)),
                style: StrokeStyle(lineWidth: 2.2, lineCap: .round, lineJoin: .round))
            VignetteKit.bar(&layer, CGRect(x: hud.midX - 19, y: hud.maxY - 10, width: 38, height: 2.2), VignettePalette.dim)
        }

        // The thumb, on the checkmark for as long as the ring takes.
        let present = ease(c, down - 0.24, down - 0.1) * (1 - ease(c, up + 0.04, up + 0.2))
        PhoneKit.touch(&ctx, at: CGPoint(x: checkmark.x - 1, y: checkmark.y + 3), presence: present, down: held)
    }

    /// One of the reader's glass discs.
    private static func chrome(
        _ ctx: inout GraphicsContext, at center: CGPoint, symbol: String, color: Color, pressed: Double
    ) {
        let disc = Path(ellipseIn: CGRect(x: center.x - 11, y: center.y - 11, width: 22, height: 22))
        var layer = ctx
        if pressed > 0 {
            layer.addFilter(.shadow(color: VignettePalette.phosphor.opacity(0.7 * pressed), radius: 5))
        }
        layer.fill(disc, with: .color(VignettePalette.cap))
        layer.fill(disc, with: .color(VignettePalette.phosphor.opacity(0.22 * pressed)))
        ctx.stroke(disc, with: .color(Color.white.opacity(0.14)), lineWidth: 0.5)
        PhoneKit.glyph(&ctx, symbol, at: center, size: 9, color: color, weight: .bold)
    }

    /// The pile: two edges under a card that names the sender and lists its
    /// mail, each line taking a check in turn once the ring has closed.
    private static func drawPile(
        _ ctx: inout GraphicsContext, _ stack: (tint: Int, name: CGFloat, rows: [CGFloat]), c: Double,
        dy: CGFloat, opacity: Double, checks: Bool
    ) {
        guard opacity > 0.001 else { return }
        let ease = VignetteClock.ease
        var layer = ctx
        layer.opacity *= opacity
        layer.translateBy(x: 0, y: dy)
        let lit = checks ? ease(c, ringTo - 0.04, ringTo + 0.1) : 0

        for depth in [2, 1] {
            let inset = CGFloat(depth) * 7
            let edge = CGRect(
                x: pile.minX + inset, y: pile.minY + CGFloat(depth) * 5, width: pile.width - 2 * inset,
                height: pile.height)
            let shape = Path(roundedRect: edge, cornerRadius: 9, style: .continuous)
            layer.fill(shape, with: .color(VignettePalette.card.opacity(depth == 1 ? 0.75 : 0.45)))
            layer.stroke(shape, with: .color(VignettePalette.edge), lineWidth: 0.5)
        }
        PhoneKit.surface(&layer, pile, radius: 9, lit: lit)

        VignetteKit.avatar(&layer, center: CGPoint(x: pile.minX + 17, y: pile.minY + 17), diameter: 14, tint: stack.tint)
        VignetteKit.bar(
            &layer, CGRect(x: pile.minX + 30, y: pile.minY + 12.5, width: stack.name, height: 3.4),
            VignettePalette.ink.opacity(0.85))
        VignetteKit.bar(&layer, CGRect(x: pile.minX + 30, y: pile.minY + 19.5, width: 28, height: 2.4), VignettePalette.dim)

        for (index, width) in stack.rows.enumerated() {
            let y = pile.minY + 42 + CGFloat(index) * 15
            let from = ringTo + 0.02 + 0.07 * Double(index)
            let check = checks ? ease(c, from, from + 0.12) : 0
            layer.fill(
                Path(CGRect(x: pile.minX + 12, y: y - 6, width: pile.width - 24, height: 0.5)),
                with: .color(VignettePalette.edge))
            VignetteKit.bar(
                &layer, CGRect(x: pile.minX + 12, y: y, width: width, height: 3),
                VignettePalette.ink.opacity(VignetteClock.mix(0.7, 0.32, check)))
            VignetteKit.bar(
                &layer, CGRect(x: pile.maxX - 30, y: y + 0.2, width: 14, height: 2.6), VignettePalette.dim.opacity(1 - check))
            VignetteKit.check(&layer, center: CGPoint(x: pile.maxX - 17, y: y + 1.5), size: 3.4, progress: check)
        }
    }
}

// MARK: - remind me later

/// REMIND ME LATER: the dashboard's For Your Eyes. The thumb drags the first
/// row to the right and the row's rail is under it: reply, remind, archive,
/// remind in its amber. The thumb lifts, presses remind, and the sheet that
/// asks when comes up; it picks the first answer. The sheet goes, and so does
/// the row: parked, the rows under it closing up.
///
/// Then the time it named comes round. The rows part and the mail is back at
/// the top where it was, its bell lit for a moment, which is the frame the
/// loop began on.
enum RemindScene: VignetteDrawing {
    static let canvas = PhoneKit.canvas
    static let loop: Double = 6.0

    private static let dragFrom: Double = 0.55
    private static let dragTo: Double = 1.05
    private static let tapRemind: Double = 1.85
    private static let closeFrom: Double = 2.0
    private static let tapWhen: Double = 3.0
    private static let sheetDown: Double = 3.2
    private static let parkFrom: Double = 3.35
    private static let returnFrom: Double = 4.6

    /// The rail out and remind pressed.
    static let posterTime: Double = tapRemind + 0.1

    private static let zone = CGRect(x: 12, y: 22, width: 196, height: 118)
    private static let rowX: CGFloat = 20
    private static let rowWidth: CGFloat = 180
    private static let rowHeight: CGFloat = 26
    private static let pitch: CGFloat = 28
    private static let firstRow: CGFloat = 50
    private static let button: CGFloat = 28
    private static let sheet = CGRect(x: 14, y: 108, width: 192, height: 82)

    private static let rail: [(symbol: String, color: Color)] = [
        ("arrowshape.turn.up.left", VignettePalette.phosphor),
        ("bell", PhoneKit.remind),
        ("archivebox", PhoneKit.neutral),
    ]

    static func draw(_ ctx: inout GraphicsContext, t: Double) {
        let ease = VignetteClock.ease
        let out = ease(t, dragFrom, dragTo) - ease(t, closeFrom, closeFrom + 0.32)
        let asked = ease(t, closeFrom + 0.04, closeFrom + 0.4) - ease(t, sheetDown, sheetDown + 0.32)
        let parked = ease(t, parkFrom, parkFrom + 0.32) - ease(t, returnFrom + 0.12, returnFrom + 0.5)
        let closedUp = ease(t, parkFrom + 0.16, parkFrom + 0.52) - ease(t, returnFrom, returnFrom + 0.4)
        let rung = ease(t, returnFrom + 0.2, returnFrom + 0.5) * (1 - ease(t, returnFrom + 0.85, returnFrom + 1.2))
        let pull = button * 3 * CGFloat(out)

        // The zone: its glyph, its name, and how many are waiting.
        PhoneKit.surface(&ctx, zone, radius: 11)
        PhoneKit.glyph(&ctx, "eye", at: CGPoint(x: zone.minX + 15, y: zone.minY + 14), size: 7.5, color: VignettePalette.phosphor)
        VignetteKit.bar(&ctx, CGRect(x: zone.minX + 25, y: zone.minY + 12.4, width: 44, height: 3.2), VignettePalette.ink.opacity(0.85))
        let count = CGRect(x: zone.minX + 74, y: zone.minY + 9.5, width: 13, height: 9)
        ctx.fill(Path(roundedRect: count, cornerRadius: 4.5), with: .color(VignettePalette.faint))
        VignetteKit.bar(&ctx, CGRect(x: count.midX - 2, y: count.midY - 1.1, width: 4, height: 2.2), VignettePalette.dim)

        // The rows under the first, closing up while it is parked.
        var rows = ctx
        rows.clip(to: Path(roundedRect: zone.insetBy(dx: 4, dy: 0), cornerRadius: 8))
        for index in 1..<3 {
            row(&rows, index, y: firstRow + CGFloat(index) * pitch - pitch * CGFloat(closedUp), dx: 0, opacity: 1, bell: 0)
        }

        // The first row's rail, standing where the row was, and the row on
        // top of it, pulled aside.
        let y = firstRow - 6 * CGFloat(parked)
        if out > 0.01 {
            var under = rows
            under.clip(to: Path(roundedRect: CGRect(x: rowX, y: y, width: max(0.1, pull), height: rowHeight), cornerRadius: 6))
            for (index, verb) in rail.enumerated() {
                let rect = CGRect(x: rowX + CGFloat(index) * button, y: y, width: button, height: rowHeight)
                let down = index == 1 ? VignetteClock.press(t, at: tapRemind) : 0
                under.fill(Path(rect), with: .color(verb.color))
                under.fill(Path(rect), with: .color(Color.white.opacity(0.22 * down)))
                PhoneKit.glyph(&under, verb.symbol, at: CGPoint(x: rect.midX, y: rect.midY - 3), size: 7.5, color: .white)
                VignetteKit.bar(
                    &under, CGRect(x: rect.midX - 6, y: rect.midY + 5.5, width: 12, height: 2), Color.white.opacity(0.85))
            }
        }
        row(&rows, 0, y: y, dx: pull, opacity: 1 - parked, bell: rung)

        // The sheet that asks when: its bell and name, three answers, the
        // first one taken.
        if asked > 0.01 {
            var layer = ctx
            layer.opacity *= min(1, asked * 1.6)
            layer.translateBy(x: 0, y: 34 * CGFloat(1 - asked))
            var lifted = layer
            lifted.addFilter(.shadow(color: Color.black.opacity(0.5), radius: 8, y: 2))
            lifted.fill(Path(roundedRect: sheet, cornerRadius: 12, style: .continuous), with: .color(VignettePalette.cap))
            layer.stroke(
                Path(roundedRect: sheet, cornerRadius: 12, style: .continuous), with: .color(Color.white.opacity(0.14)),
                lineWidth: 0.5)
            VignetteKit.bar(&layer, CGRect(x: sheet.midX - 10, y: sheet.minY + 5, width: 20, height: 2.2), Color.white.opacity(0.22))
            PhoneKit.glyph(&layer, "bell", at: CGPoint(x: sheet.minX + 14, y: sheet.minY + 18), size: 7, color: VignettePalette.phosphor)
            VignetteKit.bar(&layer, CGRect(x: sheet.minX + 23, y: sheet.minY + 16.5, width: 30, height: 3), VignettePalette.ink.opacity(0.85))
            let labels: [CGFloat] = [34, 46, 58]
            let picked = ease(t, tapWhen, tapWhen + 0.1)
            for (index, label) in labels.enumerated() {
                let option = CGRect(x: sheet.minX + 6, y: sheet.minY + 28 + CGFloat(index) * 16, width: sheet.width - 12, height: 14)
                if index == 0 {
                    let shape = Path(roundedRect: option, cornerRadius: 5, style: .continuous)
                    layer.fill(shape, with: .color(VignettePalette.phosphor.opacity(0.1 + 0.2 * picked)))
                    layer.stroke(shape, with: .color(VignettePalette.phosphor.opacity(0.35 + 0.55 * picked)), lineWidth: 0.6)
                }
                VignetteKit.bar(
                    &layer, CGRect(x: option.minX + 8, y: option.midY - 1.4, width: label, height: 2.8),
                    VignettePalette.ink.opacity(0.82))
                VignetteKit.bar(
                    &layer, CGRect(x: option.maxX - 8 - 36, y: option.midY - 1.2, width: 36, height: 2.4),
                    VignettePalette.phosphor.opacity(0.8))
            }
        }

        // The thumb: across the row, then on remind, then on an answer.
        let dragging = ease(t, dragFrom - 0.25, dragFrom - 0.1) * (1 - ease(t, dragTo + 0.16, dragTo + 0.34))
        PhoneKit.touch(
            &ctx, at: CGPoint(x: VignetteClock.mix(70, 70 + button * 3, ease(t, dragFrom, dragTo)), y: firstRow + rowHeight / 2 + 3),
            presence: dragging, down: VignetteClock.hold(t, from: dragFrom - 0.08, to: dragTo + 0.14))
        let onRemind = ease(t, tapRemind - 0.34, tapRemind - 0.18) * (1 - ease(t, tapRemind + 0.2, tapRemind + 0.36))
        PhoneKit.touch(
            &ctx, at: CGPoint(x: rowX + button * 1.5, y: firstRow + rowHeight / 2 + 2), presence: onRemind,
            down: VignetteClock.press(t, at: tapRemind))
        let onWhen = ease(t, tapWhen - 0.34, tapWhen - 0.18) * (1 - ease(t, tapWhen + 0.2, tapWhen + 0.36))
        PhoneKit.touch(
            &ctx, at: CGPoint(x: sheet.minX + 70, y: sheet.minY + 37), presence: onWhen,
            down: VignetteClock.press(t, at: tapWhen))
    }

    /// One row of the zone: who, what, a second line, and when. `bell` lights
    /// the mark a reminder comes back with, in place of the time.
    private static func row(
        _ ctx: inout GraphicsContext, _ index: Int, y: CGFloat, dx: CGFloat, opacity: Double, bell: Double
    ) {
        guard opacity > 0.01 else { return }
        let titles: [CGFloat] = [62, 48, 70]
        let subs: [CGFloat] = [112, 124, 96]
        var layer = ctx
        layer.opacity *= opacity
        let rect = CGRect(x: rowX + dx, y: y, width: rowWidth, height: rowHeight)
        // Its own ground, so a row pulled aside covers its rail.
        layer.fill(Path(roundedRect: rect, cornerRadius: 6, style: .continuous), with: .color(VignettePalette.card))
        layer.fill(Path(roundedRect: rect, cornerRadius: 6, style: .continuous), with: .color(VignettePalette.faint))
        VignetteKit.avatar(&layer, center: CGPoint(x: rect.minX + 12, y: rect.midY), diameter: 12, tint: index)
        VignetteKit.bar(
            &layer, CGRect(x: rect.minX + 24, y: rect.minY + 8, width: titles[index % 3], height: 3.2),
            VignettePalette.ink.opacity(0.85))
        VignetteKit.bar(
            &layer, CGRect(x: rect.minX + 24, y: rect.minY + 15.5, width: subs[index % 3], height: 2.6),
            VignettePalette.dim)
        VignetteKit.bar(
            &layer, CGRect(x: rect.maxX - 20, y: rect.minY + 8.3, width: 12, height: 2.4),
            VignettePalette.dim.opacity(1 - bell))
        if bell > 0.01 {
            var glow = layer
            glow.opacity *= bell
            glow.addFilter(.shadow(color: PhoneKit.remind.opacity(0.8), radius: 4))
            PhoneKit.glyph(&glow, "bell.fill", at: CGPoint(x: rect.maxX - 14, y: rect.minY + 9.5), size: 7.5, color: PhoneKit.remind)
        }
    }
}
