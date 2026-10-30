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
        case .smartClear: SmartClearPhoneScene.self
        case .calendar: CalendarPhoneScene.self
        case .quickLookDock: QuickLookDockScene.self
        case .eventReminder: EventReminderScene.self
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

// MARK: - smart clear

/// SMART CLEAR: Reading on the Sitrep, one sender's card under another, and
/// three of them have run their course. Each says why with a small mark where
/// its count would be: an offer that ended (a tag), an event that has happened
/// (a calendar), an order that shipped (a parcel). The thumb taps the zone's
/// pill, it works for a breath, and those three take their checks and go while
/// the rest stay exactly where they were. Then the receipt rises from the
/// bottom edge, where the phone's sits over the tab bar: a headline, a line
/// per reason carrying the same mark, and Undo, its ring draining to say how
/// long it lasts.
///
/// The Mac's scene of the same feature (`SmartClearScene`) on a phone's
/// column, with its marks, and a thumb for its pointer.
///
/// The seam: the receipt goes down, and new mail rises into the gaps, drawn
/// as the cards the loop began on.
enum SmartClearPhoneScene: VignetteDrawing {
    static let canvas = PhoneKit.canvas

    /// The receipt up with its three lines, the gaps it accounts for.
    static let posterTime: Double = 2.95

    private enum Reason { case offer, event, parcel }
    private struct Card {
        let tint: Int
        let name: CGFloat
        let lines: (CGFloat, CGFloat)
        let reason: Reason?
    }

    /// The expired ones are wherever they happen to be, not lined up for the
    /// camera.
    private static let cards: [Card] = [
        Card(tint: 0, name: 46, lines: (128, 96), reason: nil),
        Card(tint: 2, name: 34, lines: (112, 136), reason: .offer),
        Card(tint: 1, name: 52, lines: (140, 84), reason: .event),
        Card(tint: 2, name: 40, lines: (104, 124), reason: nil),
        Card(tint: 0, name: 30, lines: (132, 100), reason: .parcel),
    ]
    private static let rowX: CGFloat = 12
    private static let rowWidth: CGFloat = 196
    private static let rowHeight: CGFloat = 28
    private static let pitch: CGFloat = 32
    private static let firstRow: CGFloat = 34
    private static let pill = CGRect(x: 138, y: 10, width: 70, height: 16)
    /// The receipt, the screen's width, sitting where the tab bar's edge is.
    private static let receipt = CGRect(x: 12, y: 104, width: 196, height: 88)

    // The beats: the Mac's, a touch later for the thumb's approach.
    private static let tap: Double = 0.6
    private static let checks: [Double] = [1.25, 1.37, 1.49]
    private static let goneFrom: Double = 1.67
    private static let receiptIn: Double = 1.98
    private static let receiptOut: Double = 3.85
    private static let refill: Double = 4.12

    static func draw(_ ctx: inout GraphicsContext, t: Double) {
        let ease = VignetteClock.ease

        // The zone's heading: its glyph, its name, how many senders.
        PhoneKit.glyph(&ctx, "envelope.open", at: CGPoint(x: 19, y: 18), size: 7.5, color: VignettePalette.phosphor)
        VignetteKit.bar(&ctx, CGRect(x: 28, y: 16.4, width: 40, height: 3.2), VignettePalette.ink.opacity(0.85))
        let count = CGRect(x: 73, y: 13.5, width: 13, height: 9)
        ctx.fill(Path(roundedRect: count, cornerRadius: 4.5), with: .color(VignettePalette.faint))
        VignetteKit.bar(&ctx, CGRect(x: count.midX - 2, y: count.midY - 1.1, width: 4, height: 2.2), VignettePalette.dim)
        drawPill(&ctx, t: t)

        var expiredSeen = 0
        for (index, card) in cards.enumerated() {
            let origin = CGPoint(x: rowX, y: firstRow + CGFloat(index) * pitch)
            guard card.reason != nil else {
                drawCard(&ctx, card, at: origin, opacity: 1, dy: 0, check: 0)
                continue
            }
            let at = checks[expiredSeen]
            expiredSeen += 1
            let check = ease(t, at, at + 0.14)
            let gone = ease(t, goneFrom, goneFrom + 0.3)
            let from = refill + 0.06 * Double(expiredSeen)
            let back = ease(t, from, from + 0.34)
            if gone < 1 {
                drawCard(&ctx, card, at: origin, opacity: 1 - gone, dy: -6 * CGFloat(gone), check: check)
            } else if back > 0 {
                drawCard(&ctx, card, at: origin, opacity: back, dy: 8 * CGFloat(1 - back), check: 0)
            }
        }
        drawReceipt(&ctx, t: t)

        // The thumb, once on the pill.
        let on = ease(t, tap - 0.34, tap - 0.18) * (1 - ease(t, tap + 0.22, tap + 0.4))
        PhoneKit.touch(
            &ctx, at: CGPoint(x: pill.midX + 4, y: pill.midY + 3), presence: on,
            down: VignetteClock.press(t, at: tap))
    }

    /// The zone's pill: its check-stack glyph and its name, then the app's
    /// spinner in the glyph's place while it tidies up.
    private static func drawPill(_ ctx: inout GraphicsContext, t: Double) {
        let ease = VignetteClock.ease
        let pressed = VignetteClock.press(t, at: tap)
        VignetteKit.pill(&ctx, pill, label: 36, labelX: pill.minX + 22, pressed: pressed)
        let working = ease(t, tap + 0.12, tap + 0.22) - ease(t, checks[0] - 0.1, checks[0])
        let mark = CGPoint(x: pill.minX + 12, y: pill.midY + CGFloat(pressed))
        var glyph = ctx
        glyph.opacity *= 1 - working
        PhoneKit.glyph(
            &glyph, "checkmark.rectangle.stack", at: mark, size: 7,
            color: pressed > 0.5 ? Color.white : VignettePalette.phosphorHot)
        VignetteKit.arc(
            &ctx, center: mark, radius: 3.4, head: t * 1.6, sweep: 0.3, color: VignettePalette.phosphorHot,
            lineWidth: 1, opacity: working)
    }

    /// The reason a card has run its course, as its mark.
    private static func reasonMark(_ ctx: inout GraphicsContext, _ reason: Reason, center: CGPoint, size: CGFloat, color: Color) {
        switch reason {
        case .event: VignetteKit.calendarMark(&ctx, center: center, size: size, color: color)
        case .offer: VignetteKit.tagMark(&ctx, center: center, size: size, color: color)
        case .parcel: VignetteKit.parcelMark(&ctx, center: center, size: size, color: color)
        }
    }

    /// One sender's card, after ReadingCard: a square hero on the left, the
    /// name and its count across the top, two lines of summary under. An
    /// expired card wears its reason where the count would be.
    private static func drawCard(
        _ ctx: inout GraphicsContext, _ card: Card, at origin: CGPoint, opacity: Double, dy: CGFloat, check: Double
    ) {
        guard opacity > 0.001 else { return }
        var layer = ctx
        layer.opacity *= opacity
        layer.translateBy(x: 0, y: dy)
        let rect = CGRect(origin: origin, size: CGSize(width: rowWidth, height: rowHeight))
        PhoneKit.surface(&layer, rect, lit: check)
        let hero = CGRect(x: rect.minX + 5, y: rect.minY + 5, width: 18, height: 18)
        layer.fill(
            Path(roundedRect: hero, cornerRadius: 4, style: .continuous),
            with: .color(VignettePalette.avatar[card.tint].opacity(VignetteClock.mix(0.5, 0.28, check))))
        let x = rect.minX + 30
        VignetteKit.bar(
            &layer, CGRect(x: x, y: rect.minY + 6.5, width: card.name, height: 3.2),
            VignettePalette.ink.opacity(VignetteClock.mix(0.85, 0.4, check)))
        VignetteKit.bar(&layer, CGRect(x: x, y: rect.minY + 14, width: card.lines.0, height: 2.4), VignettePalette.dim)
        VignetteKit.bar(&layer, CGRect(x: x, y: rect.minY + 19.5, width: card.lines.1, height: 2.4), VignettePalette.dim)
        let corner = CGPoint(x: rect.maxX - 11, y: rect.minY + 8.5)
        if let reason = card.reason {
            reasonMark(&layer, reason, center: corner, size: 8, color: VignettePalette.ink.opacity(0.65 * (1 - check)))
            VignetteKit.check(&layer, center: corner, size: 3.6, progress: check)
        } else {
            VignetteKit.bar(&layer, CGRect(x: rect.maxX - 17, y: rect.minY + 7.3, width: 10, height: 2.4), VignettePalette.dim)
        }
    }

    /// The receipt, after SmartClearReceiptCard on a phone: its name, how many
    /// went, from how many senders, a line per reason (the mark, a count, the
    /// words), a rule, and Undo with its ring, draining.
    private static func drawReceipt(_ ctx: inout GraphicsContext, t: Double) {
        let ease = VignetteClock.ease
        let arrive = ease(t, receiptIn, receiptIn + 0.34)
        let leave = ease(t, receiptOut, receiptOut + 0.3)
        guard arrive > 0, leave < 1 else { return }
        // One layer, faded whole: parts faded one by one let the card under
        // the receipt show through each of them in turn.
        var outer = ctx
        outer.opacity *= arrive * (1 - leave)
        outer.translateBy(x: 0, y: 22 * CGFloat(1 - arrive) + 16 * CGFloat(leave))
        outer.drawLayer { layer in drawReceiptBody(&layer, t: t) }
    }

    private static func drawReceiptBody(_ layer: inout GraphicsContext, t: Double) {
        let ease = VignetteClock.ease
        let shape = Path(roundedRect: receipt, cornerRadius: 12, style: .continuous)
        var lifted = layer
        lifted.addFilter(.shadow(color: Color.black.opacity(0.5), radius: 8, y: 2))
        lifted.fill(shape, with: .color(VignettePalette.cap))
        layer.stroke(shape, with: .color(Color.white.opacity(0.14)), lineWidth: 0.5)

        let x = receipt.minX + 12
        VignetteKit.check(&layer, center: CGPoint(x: x + 2.6, y: receipt.minY + 11), size: 2.6, progress: 1, opacity: 0.8)
        VignetteKit.bar(&layer, CGRect(x: x + 9, y: receipt.minY + 9.8, width: 30, height: 2.4), VignettePalette.dim)
        VignetteKit.bar(&layer, CGRect(x: x, y: receipt.minY + 18, width: 62, height: 5.4), VignettePalette.ink.opacity(0.9))
        VignetteKit.bar(&layer, CGRect(x: x, y: receipt.minY + 27.5, width: 34, height: 2.4), VignettePalette.dim)

        let reasons: [(Reason, CGFloat)] = [(.offer, 44), (.event, 56), (.parcel, 50)]
        for (index, line) in reasons.enumerated() {
            let from = receiptIn + 0.24 + 0.1 * Double(index)
            var row = layer
            row.opacity *= ease(t, from, from + 0.2)
            let y = receipt.minY + 39 + CGFloat(index) * 9
            reasonMark(&row, line.0, center: CGPoint(x: x + 3.5, y: y), size: 6.4, color: VignettePalette.phosphorHot)
            VignetteKit.bar(&row, CGRect(x: x + 12, y: y - 1.4, width: 5, height: 2.8), VignettePalette.ink.opacity(0.85))
            VignetteKit.bar(&row, CGRect(x: x + 20, y: y - 1.2, width: line.1, height: 2.4), VignettePalette.dim)
        }

        // The rule, then Undo: a soft capsule with its name and the ring
        // that says how long it has left, and the quiet line after it.
        layer.fill(
            Path(CGRect(x: receipt.minX + 12, y: receipt.minY + 65, width: receipt.width - 24, height: 0.5)),
            with: .color(VignettePalette.edge))
        let undo = CGRect(x: x, y: receipt.minY + 70, width: 46, height: 13)
        let capsule = Path(roundedRect: undo, cornerRadius: undo.height / 2, style: .continuous)
        layer.fill(capsule, with: .color(VignettePalette.phosphor.opacity(0.18)))
        layer.stroke(capsule, with: .color(VignettePalette.phosphor.opacity(0.4)), lineWidth: 0.6)
        VignetteKit.bar(&layer, CGRect(x: undo.minX + 8, y: undo.midY - 1.3, width: 18, height: 2.6), VignettePalette.ink.opacity(0.9))
        let ring = CGPoint(x: undo.maxX - 10, y: undo.midY)
        let left = 1 - VignetteClock.ramp(t, receiptIn + 0.3, receiptOut + 0.3)
        VignetteKit.ring(&layer, center: ring, radius: 3.2, progress: 1, color: Color.white.opacity(0.16), lineWidth: 1.1)
        VignetteKit.ring(&layer, center: ring, radius: 3.2, progress: left, color: VignettePalette.phosphor, lineWidth: 1.1)
        VignetteKit.bar(
            &layer, CGRect(x: receipt.maxX - 12 - 54, y: undo.midY - 1.1, width: 54, height: 2.2),
            VignettePalette.dim.opacity(0.7))
    }
}

// MARK: - the calendar tab

/// CALENDAR: the phone's new tab, open on the month, which is where it opens
/// the first time and wherever you left it after. Each day is a date and a
/// few dots in its calendars' colours. The thumb taps a day with two plans
/// on it and the month gives way to that day as hours, which scroll by
/// themselves to its first plan. The thumb taps an empty stretch of the
/// afternoon: a new hour is outlined there and the event sheet comes up over
/// it. Its title is written, two guests join the guest line, and save goes
/// down. The sheet goes, the hour fills in its calendar's colour, and the
/// mode glyph in the corner takes the thumb back to the month, where the day
/// has a third dot.
///
/// Blue is state here, as in the app (today, the outline of an event being
/// made), never a calendar: the plans wear the avatar tints.
///
/// The seam: the month turns to the next, drawn as the one the loop began on.
enum CalendarPhoneScene: VignetteDrawing {
    static let canvas = PhoneKit.canvas
    static let loop: Double = 7.0

    private static let tapDay: Double = 0.6
    private static let toDayFrom: Double = 0.74
    private static let toDayTo: Double = 1.14
    private static let scrollFrom: Double = 1.3
    private static let scrollTo: Double = 1.95
    private static let tapGrid: Double = 2.4
    private static let sheetUp: Double = 2.62
    private static let typeFrom: Double = 3.0
    private static let typeTo: Double = 3.38
    private static let guests: [Double] = [3.46, 3.68]
    private static let tapSave: Double = 4.26
    private static let sheetDown: Double = 4.4
    private static let tapMode: Double = 5.05
    private static let toMonthFrom: Double = 5.19
    private static let toMonthTo: Double = 5.59
    private static let turnFrom: Double = 6.1
    private static let turnTo: Double = 6.66

    /// The day open, the sheet up with its title and both guests.
    static let posterTime: Double = 3.9

    // The month: seven columns, five weeks.
    private static let gridX: CGFloat = 10
    private static let cellW: CGFloat = 200 / 7
    private static let gridTop: CGFloat = 42
    private static let cellH: CGFloat = 30
    private static let today = 9
    private static let picked = 18
    /// Each day's dots, by cell: avatar tints, up to three.
    private static let dots: [Int: [Int]] = [
        3: [1], 5: [2, 1], 8: [2], 9: [1], 12: [1, 2], 15: [2], 18: [2, 1], 20: [1],
        23: [2, 2, 1], 26: [1], 29: [2], 31: [1, 2],
    ]
    private static let modeButton = CGPoint(x: 194, y: 16)

    // The day: hours under a week strip.
    private static let hoursTop: CGFloat = 56
    private static let hourH: CGFloat = 24
    private static let scrolled: CGFloat = 4.5 * hourH
    /// How much further the hours go once the sheet is up.
    private static let lift: CGFloat = 55

    private struct Plan {
        /// In hours from the top of the day's list.
        let start: CGFloat
        let length: CGFloat
        let tint: Int
        let title: CGFloat
    }

    private static let plans = [Plan(start: 5, length: 1.5, tint: 2, title: 64), Plan(start: 8.5, length: 1, tint: 1, title: 52)]
    private static let made = Plan(start: 7, length: 1, tint: 1, title: 58)
    private static let sheet = CGRect(x: 12, y: 92, width: 196, height: 100)

    static func draw(_ ctx: inout GraphicsContext, t: Double) {
        let ease = VignetteClock.ease
        let inDay = ease(t, toDayFrom, toDayTo) - ease(t, toMonthFrom, toMonthTo)
        let back = t >= toMonthFrom

        // The month: the one the loop began on, then (after the day) the same
        // month with the new dot, turning away as the next comes in.
        if inDay < 1 {
            var month = ctx
            // One goes before the other comes: two screens at half strength
            // on top of each other is neither of them.
            month.opacity *= 1 - ease(inDay, 0, 0.6)
            if back {
                let turn = ease(t, turnFrom, turnTo)
                let gone = ease(turn, 0, 0.6), come = ease(turn, 0.4, 1)
                if gone < 1 {
                    var leaving = month
                    leaving.opacity *= 1 - gone
                    leaving.translateBy(x: -60 * CGFloat(turn), y: 0)
                    drawMonth(&leaving, t: t, returned: true)
                }
                if come > 0 {
                    var arriving = month
                    arriving.opacity *= come
                    arriving.translateBy(x: 60 * CGFloat(1 - turn), y: 0)
                    drawMonth(&arriving, t: 0, returned: false)
                }
            } else {
                // Leaving for the day, it grows a little about the day picked.
                let cell = cellRect(picked)
                let grow = 1 + 0.04 * CGFloat(inDay)
                month.translateBy(x: cell.midX, y: cell.midY)
                month.scaleBy(x: grow, y: grow)
                month.translateBy(x: -cell.midX, y: -cell.midY)
                drawMonth(&month, t: t, returned: false)
            }
        }
        if inDay > 0 {
            var day = ctx
            day.opacity *= ease(inDay, 0.4, 1)
            day.translateBy(x: 0, y: 8 * CGFloat(1 - inDay))
            drawDay(&day, t: t)
        }

        // The mode glyph: the shape of what is on screen, and the way back.
        let pressed = VignetteClock.press(t, at: tapMode)
        let disc = Path(ellipseIn: CGRect(x: modeButton.x - 10, y: modeButton.y - 10, width: 20, height: 20))
        var glow = ctx
        if pressed > 0 {
            glow.addFilter(.shadow(color: VignettePalette.phosphor.opacity(0.7 * pressed), radius: 5))
        }
        glow.fill(disc, with: .color(VignettePalette.cap))
        glow.fill(disc, with: .color(VignettePalette.phosphor.opacity(0.22 * pressed)))
        ctx.stroke(disc, with: .color(Color.white.opacity(0.14)), lineWidth: 0.5)
        var monthGlyph = ctx
        monthGlyph.opacity *= 1 - ease(inDay, 0, 0.6)
        PhoneKit.glyph(&monthGlyph, "calendar", at: modeButton, size: 8.5, color: VignettePalette.phosphor)
        var dayGlyph = ctx
        dayGlyph.opacity *= ease(inDay, 0.4, 1)
        PhoneKit.glyph(&dayGlyph, "clock", at: modeButton, size: 8.5, color: VignettePalette.phosphor)

        drawSheet(&ctx, t: t)

        // The thumb: a day, the empty afternoon, save, the mode glyph.
        let dayAt = cellRect(picked)
        thumb(&ctx, t: t, at: tapDay, point: CGPoint(x: dayAt.midX + 2, y: dayAt.midY + 3))
        thumb(&ctx, t: t, at: tapGrid, point: CGPoint(x: 128, y: hourY(made.start + 0.45) + 3))
        thumb(&ctx, t: t, at: tapSave, point: CGPoint(x: savePill.midX + 2, y: savePill.midY + 3))
        thumb(&ctx, t: t, at: tapMode, point: CGPoint(x: modeButton.x - 1, y: modeButton.y + 3))
    }

    private static func thumb(_ ctx: inout GraphicsContext, t: Double, at: Double, point: CGPoint) {
        let ease = VignetteClock.ease
        let on = ease(t, at - 0.34, at - 0.18) * (1 - ease(t, at + 0.22, at + 0.4))
        PhoneKit.touch(&ctx, at: point, presence: on, down: VignetteClock.press(t, at: at))
    }

    private static func cellRect(_ index: Int) -> CGRect {
        CGRect(
            x: gridX + CGFloat(index % 7) * cellW, y: gridTop + CGFloat(index / 7) * cellH, width: cellW,
            height: cellH)
    }

    /// A date, as a bar: one figure or two.
    private static func dateWidth(_ index: Int) -> CGFloat {
        let n = index < 2 ? 29 + index : (index > 32 ? index - 32 : index - 1)
        return n < 10 ? 3.2 : 5.8
    }

    /// The month's name, and its year, softer.
    private static func heading(_ ctx: inout GraphicsContext) {
        VignetteKit.bar(&ctx, CGRect(x: 14, y: 13.8, width: 50, height: 4.2), VignettePalette.ink.opacity(0.9))
        VignetteKit.bar(&ctx, CGRect(x: 68, y: 14.6, width: 18, height: 2.8), VignettePalette.dim)
    }

    /// The month's grid. `returned` is the month after the day was visited:
    /// the day still picked, and the new plan's dot, lit as it arrives.
    private static func drawMonth(_ ctx: inout GraphicsContext, t: Double, returned: Bool) {
        let ease = VignetteClock.ease
        heading(&ctx)
        for column in 0..<7 {
            let x = gridX + cellW * (CGFloat(column) + 0.5)
            VignetteKit.bar(&ctx, CGRect(x: x - 3, y: 32, width: 6, height: 2), VignettePalette.dim)
        }
        let pressed = returned ? 0 : VignetteClock.press(t, at: tapDay)
        let held = returned ? 1 : ease(t, tapDay + 0.04, tapDay + 0.14)
        for index in 0..<35 {
            let cell = cellRect(index)
            if index % 7 == 0 {
                ctx.fill(
                    Path(CGRect(x: gridX, y: cell.minY, width: cellW * 7, height: 0.5)),
                    with: .color(VignettePalette.edge))
            }
            if index == picked, held > 0 {
                let well = Path(roundedRect: cell.insetBy(dx: 1.5, dy: 2.5), cornerRadius: 5, style: .continuous)
                var lit = ctx
                lit.opacity *= held
                lit.fill(well, with: .color(VignettePalette.phosphor.opacity(0.14 + 0.14 * pressed)))
                lit.stroke(well, with: .color(VignettePalette.phosphor.opacity(0.5)), lineWidth: 0.5)
            }
            let outside = index < 2 || index > 32
            let date = CGRect(x: cell.minX + 5, y: cell.minY + 6.5, width: dateWidth(index), height: 2.6)
            if index == today {
                ctx.fill(
                    Path(ellipseIn: CGRect(x: date.midX - 5.5, y: date.midY - 5.5, width: 11, height: 11)),
                    with: .color(VignettePalette.phosphor))
                VignetteKit.bar(&ctx, date, Color.white.opacity(0.95))
            } else {
                VignetteKit.bar(&ctx, date, outside ? VignettePalette.dim.opacity(0.6) : VignettePalette.ink.opacity(0.78))
            }
            var tints = dots[index] ?? []
            let fresh = index == picked && returned
            if fresh { tints.append(made.tint) }
            for (n, tint) in tints.enumerated() {
                let center = CGPoint(x: cell.minX + 6.6 + CGFloat(n) * 5.4, y: cell.minY + 19)
                if fresh, n == tints.count - 1 {
                    let arrive = ease(t, toMonthTo - 0.1, toMonthTo + 0.2)
                    let glow = 1 - ease(t, toMonthTo + 0.3, toMonthTo + 0.7)
                    var dot = ctx
                    dot.opacity *= arrive
                    dot.addFilter(.shadow(color: VignettePalette.phosphor.opacity(0.9 * glow), radius: 3))
                    VignetteKit.avatar(&dot, center: center, diameter: 3.4, tint: tint)
                } else {
                    VignetteKit.avatar(&ctx, center: center, diameter: 3.4, tint: tint, opacity: outside ? 0.4 : 0.9)
                }
            }
        }
    }

    /// Where an hour sits on screen with the list scrolled by `scroll`.
    private static func hourY(_ hours: CGFloat, scroll: CGFloat = scrolled) -> CGFloat {
        hoursTop + 3 + hours * hourH - scroll
    }

    /// The day: its week as a strip with the day picked, then its hours.
    private static func drawDay(_ ctx: inout GraphicsContext, t: Double) {
        let ease = VignetteClock.ease
        heading(&ctx)
        for column in 0..<7 {
            let index = 14 + column
            let x = gridX + cellW * (CGFloat(column) + 0.5)
            VignetteKit.bar(&ctx, CGRect(x: x - 3, y: 31, width: 6, height: 2), VignettePalette.dim)
            let width = dateWidth(index)
            let date = CGRect(x: x - width / 2, y: 40.7, width: width, height: 2.6)
            if index == picked {
                ctx.fill(
                    Path(ellipseIn: CGRect(x: x - 6.5, y: date.midY - 6.5, width: 13, height: 13)),
                    with: .color(VignettePalette.phosphor))
                VignetteKit.bar(&ctx, date, Color.white.opacity(0.95))
            } else {
                VignetteKit.bar(&ctx, date, VignettePalette.ink.opacity(0.7))
            }
        }
        ctx.fill(Path(CGRect(x: 0, y: hoursTop - 3, width: canvas.width, height: 0.5)), with: .color(VignettePalette.edge))

        // The hours, scrolled to the first plan by themselves, under a fade
        // at the foot so the list runs on rather than stopping.
        // and once the sheet comes up, a little further, so the hour being
        // made stays in sight above it.
        let scroll = scrolled * CGFloat(ease(t, scrollFrom, scrollTo))
            + lift * CGFloat(ease(t, sheetUp - 0.04, sheetUp + 0.4))
        let outlined = ease(t, tapGrid + 0.04, tapGrid + 0.24)
        let saved = ease(t, sheetDown + 0.06, sheetDown + 0.36)
        let lit = saved * (1 - ease(t, sheetDown + 0.6, sheetDown + 0.95))
        var hours = ctx
        hours.clip(to: Path(CGRect(x: 0, y: hoursTop - 2.5, width: canvas.width, height: canvas.height - hoursTop)))
        hours.drawLayer { layer in
            for hour in 0..<18 {
                let y = hourY(CGFloat(hour), scroll: scroll)
                VignetteKit.bar(&layer, CGRect(x: 12, y: y - 1.1, width: 9, height: 2.2), VignettePalette.dim)
                layer.fill(Path(CGRect(x: 28, y: y, width: 180, height: 0.5)), with: .color(VignettePalette.edge))
            }
            for plan in plans {
                block(&layer, plan, scroll: scroll, fill: 1, lit: 0)
            }
            // The new hour: an outline where the thumb went down, filled in
            // its calendar's colour once it is saved.
            if outlined > 0.01 {
                var draft = layer
                draft.opacity *= outlined
                let shape = Path(roundedRect: blockRect(made, scroll: scroll), cornerRadius: 5, style: .continuous)
                draft.fill(shape, with: .color(VignettePalette.phosphor.opacity(0.14 * (1 - saved))))
                draft.stroke(
                    shape, with: .color(VignettePalette.phosphor.opacity(0.9 * (1 - saved))),
                    style: StrokeStyle(lineWidth: 0.8, dash: [3, 2]))
                block(&draft, made, scroll: scroll, fill: saved, lit: lit)
            }
            layer.blendMode = .destinationOut
            layer.fill(
                Path(CGRect(x: 0, y: canvas.height - 26, width: canvas.width, height: 26)),
                with: .linearGradient(
                    Gradient(colors: [.clear, .white]), startPoint: CGPoint(x: 0, y: canvas.height - 26),
                    endPoint: CGPoint(x: 0, y: canvas.height)))
        }
    }

    private static func blockRect(_ plan: Plan, scroll: CGFloat) -> CGRect {
        CGRect(x: 32, y: hourY(plan.start, scroll: scroll) + 1, width: 174, height: plan.length * hourH - 2)
    }

    /// One plan on the hours: its calendar's colour, evenly edged, its title
    /// and its time.
    private static func block(_ ctx: inout GraphicsContext, _ plan: Plan, scroll: CGFloat, fill: Double, lit: Double) {
        guard fill > 0.01 else { return }
        let rect = blockRect(plan, scroll: scroll)
        let shape = Path(roundedRect: rect, cornerRadius: 5, style: .continuous)
        let tint = VignettePalette.avatar[plan.tint]
        var layer = ctx
        layer.opacity *= fill
        layer.fill(shape, with: .color(VignettePalette.card))
        layer.fill(shape, with: .color(tint.opacity(0.34)))
        layer.stroke(shape, with: .color(tint.opacity(0.55)), lineWidth: 0.6)
        if lit > 0 {
            layer.stroke(shape, with: .color(VignettePalette.phosphorHot.opacity(0.8 * lit)), lineWidth: 0.8)
        }
        VignetteKit.bar(
            &layer, CGRect(x: rect.minX + 8, y: rect.minY + 6, width: plan.title, height: 3),
            VignettePalette.ink.opacity(0.88))
        VignetteKit.bar(
            &layer, CGRect(x: rect.minX + 8, y: rect.minY + 13, width: 34, height: 2.4), VignettePalette.ink.opacity(0.45))
    }

    private static var savePill: CGRect { CGRect(x: sheet.maxX - 46, y: sheet.minY + 11, width: 34, height: 13) }

    /// The event sheet: cancel and save, the title being written, when, and
    /// the guest line, where two people join.
    private static func drawSheet(_ ctx: inout GraphicsContext, t: Double) {
        let ease = VignetteClock.ease
        let up = ease(t, sheetUp, sheetUp + 0.36) - ease(t, sheetDown, sheetDown + 0.32)
        guard up > 0.01 else { return }
        var outer = ctx
        outer.opacity *= min(1, up * 1.6)
        outer.translateBy(x: 0, y: 40 * CGFloat(1 - up))
        outer.drawLayer { layer in drawSheetBody(&layer, t: t) }
    }

    private static func drawSheetBody(_ layer: inout GraphicsContext, t: Double) {
        let ease = VignetteClock.ease
        let shape = Path(roundedRect: sheet, cornerRadius: 13, style: .continuous)
        var lifted = layer
        lifted.addFilter(.shadow(color: Color.black.opacity(0.5), radius: 8, y: 2))
        lifted.fill(shape, with: .color(VignettePalette.cap))
        layer.stroke(shape, with: .color(Color.white.opacity(0.14)), lineWidth: 0.5)
        VignetteKit.bar(
            &layer, CGRect(x: sheet.midX - 10, y: sheet.minY + 4.5, width: 20, height: 2.2), Color.white.opacity(0.22))

        // Cancel, and save.
        let x = sheet.minX + 12
        VignetteKit.bar(&layer, CGRect(x: x, y: savePill.midY - 1.2, width: 24, height: 2.4), VignettePalette.dim)
        VignetteKit.pill(
            &layer, savePill, label: 16, labelX: savePill.midX - 8, pressed: VignetteClock.press(t, at: tapSave))

        // The title, a word at a time, the caret at its end.
        let width = max(2.6, 88 * CGFloat(VignetteClock.ramp(t, typeFrom, typeTo)))
        VignetteKit.bar(
            &layer, CGRect(x: x, y: sheet.minY + 33, width: width, height: 4.2), VignettePalette.ink.opacity(0.9))
        if t < tapSave {
            layer.fill(
                Path(roundedRect: CGRect(x: x + width + 2, y: sheet.minY + 31, width: 1.1, height: 8), cornerRadius: 0.5),
                with: .color(VignettePalette.phosphorHot))
        }
        for y in [sheet.minY + 46, sheet.minY + 66] {
            layer.fill(Path(CGRect(x: x, y: y, width: sheet.width - 24, height: 0.5)), with: .color(VignettePalette.edge))
        }

        // When: the day, and the hour the thumb went down on.
        PhoneKit.glyph(&layer, "clock", at: CGPoint(x: x + 4, y: sheet.minY + 56), size: 7, color: VignettePalette.dim)
        VignetteKit.bar(
            &layer, CGRect(x: x + 14, y: sheet.minY + 54.8, width: 34, height: 2.6), VignettePalette.ink.opacity(0.75))
        VignetteKit.bar(
            &layer, CGRect(x: sheet.maxX - 12 - 40, y: sheet.minY + 54.8, width: 40, height: 2.6),
            VignettePalette.ink.opacity(0.75))

        // Guests: each one a chip, coming in as they are added.
        PhoneKit.glyph(&layer, "person.2", at: CGPoint(x: x + 4, y: sheet.minY + 80), size: 7, color: VignettePalette.dim)
        let names: [CGFloat] = [22, 28]
        var chipX = x + 14
        for (index, at) in guests.enumerated() {
            let shown = ease(t, at, at + 0.2)
            let chip = CGRect(x: chipX, y: sheet.minY + 73.5, width: 17 + names[index], height: 13)
            chipX = chip.maxX + 5
            guard shown > 0.01 else { continue }
            var one = layer
            one.opacity *= shown
            let s = VignetteClock.mix(0.93, 1, shown)
            one.translateBy(x: chip.minX, y: chip.midY)
            one.scaleBy(x: s, y: s)
            one.translateBy(x: -chip.minX, y: -chip.midY)
            let capsule = Path(roundedRect: chip, cornerRadius: chip.height / 2, style: .continuous)
            one.fill(capsule, with: .color(VignettePalette.faint))
            one.stroke(capsule, with: .color(VignettePalette.edge), lineWidth: 0.5)
            VignetteKit.avatar(&one, center: CGPoint(x: chip.minX + 7, y: chip.midY), diameter: 8, tint: index == 0 ? 2 : 0)
            VignetteKit.bar(
                &one, CGRect(x: chip.minX + 14, y: chip.midY - 1.2, width: names[index], height: 2.4),
                VignettePalette.ink.opacity(0.8))
        }
        // Where the next one would be typed.
        VignetteKit.bar(
            &layer, CGRect(x: chipX + 2, y: sheet.minY + 78.8, width: 30, height: 2.4), VignettePalette.dim.opacity(0.6))
    }
}

// MARK: - Quick Look at the foot of the board

/// QUICK LOOK AT THE FOOT OF THE BOARD: the Sitrep, the day's line and For
/// Your Eyes, and under the cards a strip that says Quick Look and what is
/// in it: a parcel, a bill and a code, each with its count. The thumb goes
/// down on the strip and drags it up; let go, it opens the rest of the way,
/// over the board, which dims under it. Inside are the records the Mac keeps
/// on its rail: packages with how far along they are, bills with what is
/// owed, and the newest login code. A breath to look, then the thumb drags
/// the header down and the strip docks where it was.
///
/// The seam is the docked board, which is where the loop began.
enum QuickLookDockScene: VignetteDrawing {
    static let canvas = PhoneKit.canvas
    static let loop: Double = 5.4

    private static let downAt: Double = 0.5
    private static let dragFrom: Double = 0.62
    private static let dragTo: Double = 1.08
    private static let liftAt: Double = 1.16
    private static let openTo: Double = 1.56
    private static let closeDown: Double = 3.3
    private static let closeFrom: Double = 3.42
    private static let closeTo: Double = 3.8
    private static let closeLift: Double = 3.88
    private static let dockTo: Double = 4.28

    /// Open over the board, every record in view.
    static let posterTime: Double = 2.4

    private static let docked: CGFloat = 158
    private static let raised: CGFloat = 8
    private static let dragged: CGFloat = 104
    private static let dropped: CGFloat = 70
    private static let foot: CGFloat = 194
    private static let marks = ["shippingbox", "creditcard", "key"]

    static func draw(_ ctx: inout GraphicsContext, t: Double) {
        let ease = VignetteClock.ease
        // Where the strip's top edge is: following the thumb, then the rest
        // of the way on its own once let go.
        let top: CGFloat
        if t < liftAt {
            top = VignetteClock.mix(docked, dragged, ease(t, dragFrom, dragTo))
        } else if t < closeDown {
            top = VignetteClock.mix(dragged, raised, ease(t, liftAt, openTo))
        } else if t < closeLift {
            top = VignetteClock.mix(raised, dropped, ease(t, closeFrom, closeTo))
        } else {
            top = VignetteClock.mix(dropped, docked, ease(t, closeLift, dockTo))
        }
        let open = Double((docked - top) / (docked - raised))

        drawBoard(&ctx)
        ctx.fill(Path(CGRect(origin: .zero, size: canvas)), with: .color(Color.black.opacity(0.32 * open)))
        drawDock(&ctx, top: top, open: open)

        // The thumb: up from the strip, then down from the header.
        let up = ease(t, downAt - 0.24, downAt - 0.1) * (1 - ease(t, liftAt, liftAt + 0.18))
        PhoneKit.touch(
            &ctx, at: CGPoint(x: 112, y: VignetteClock.mix(docked + 19, dragged + 19, ease(t, dragFrom, dragTo))),
            presence: up, down: VignetteClock.hold(t, from: downAt, to: liftAt - 0.06))
        let down = ease(t, closeDown - 0.24, closeDown - 0.1) * (1 - ease(t, closeLift, closeLift + 0.18))
        PhoneKit.touch(
            &ctx, at: CGPoint(x: 112, y: VignetteClock.mix(raised + 19, dropped + 19, ease(t, closeFrom, closeTo))),
            presence: down, down: VignetteClock.hold(t, from: closeDown, to: closeLift - 0.06))
    }

    /// The board: the day's line, then For Your Eyes and its rows.
    private static func drawBoard(_ ctx: inout GraphicsContext) {
        VignetteKit.bar(&ctx, CGRect(x: 14, y: 12, width: 34, height: 2.4), VignettePalette.dim)
        VignetteKit.bar(&ctx, CGRect(x: 14, y: 20, width: 132, height: 6), VignettePalette.ink.opacity(0.9))
        VignetteKit.bar(&ctx, CGRect(x: 14, y: 30, width: 92, height: 6), VignettePalette.ink.opacity(0.9))

        let zone = CGRect(x: 12, y: 46, width: 196, height: 104)
        PhoneKit.surface(&ctx, zone, radius: 11)
        PhoneKit.glyph(
            &ctx, "eye", at: CGPoint(x: zone.minX + 15, y: zone.minY + 13), size: 7.5, color: VignettePalette.phosphor)
        VignetteKit.bar(
            &ctx, CGRect(x: zone.minX + 25, y: zone.minY + 11.4, width: 44, height: 3.2), VignettePalette.ink.opacity(0.85))
        let titles: [CGFloat] = [62, 48, 70]
        let subs: [CGFloat] = [112, 124, 96]
        for index in 0..<3 {
            let rect = CGRect(
                x: zone.minX + 8, y: zone.minY + 25 + CGFloat(index) * 26, width: zone.width - 16, height: 22)
            if index > 0 {
                ctx.fill(
                    Path(CGRect(x: rect.minX + 4, y: rect.minY - 2, width: rect.width - 8, height: 0.5)),
                    with: .color(VignettePalette.edge))
            }
            VignetteKit.avatar(&ctx, center: CGPoint(x: rect.minX + 10, y: rect.midY), diameter: 11, tint: index)
            VignetteKit.bar(
                &ctx, CGRect(x: rect.minX + 22, y: rect.minY + 6, width: titles[index], height: 3.2),
                VignettePalette.ink.opacity(0.85))
            VignetteKit.bar(
                &ctx, CGRect(x: rect.minX + 22, y: rect.minY + 13, width: subs[index], height: 2.6), VignettePalette.dim)
            VignetteKit.bar(
                &ctx, CGRect(x: rect.maxX - 18, y: rect.minY + 6.3, width: 12, height: 2.4), VignettePalette.dim)
        }
    }

    /// The tray: its header (the name, what is in it, the chevron), and once
    /// it is open, the records.
    private static func drawDock(_ ctx: inout GraphicsContext, top: CGFloat, open: Double) {
        let ease = VignetteClock.ease
        let rect = CGRect(x: 8, y: top, width: 204, height: foot - top)
        let shape = Path(roundedRect: rect, cornerRadius: 15, style: .continuous)
        var lifted = ctx
        lifted.addFilter(.shadow(color: Color.black.opacity(0.45 + 0.15 * open), radius: 10))
        lifted.fill(shape, with: .color(VignettePalette.cap))
        ctx.fill(shape, with: .color(VignettePalette.faint))
        ctx.stroke(shape, with: .color(Color.white.opacity(0.14)), lineWidth: 0.5)

        VignetteKit.bar(&ctx, CGRect(x: rect.midX - 10, y: top + 4.5, width: 20, height: 2.2), Color.white.opacity(0.22))

        var inside = ctx
        inside.clip(to: shape)
        let x = rect.minX + 14
        // The name grows a size as the tray opens, as the app's does.
        VignetteKit.bar(
            &inside,
            CGRect(x: x, y: top + 11.5, width: VignetteClock.mix(46, 56, open), height: VignetteClock.mix(3.6, 4.6, open)),
            VignettePalette.ink.opacity(0.9))
        // Docked, what is inside in a few words: a parcel, a bill, a code.
        var summary = inside
        summary.opacity *= 1 - ease(open, 0, 0.35)
        for (index, mark) in marks.enumerated() {
            let mx = x + 3 + CGFloat(index) * 26
            PhoneKit.glyph(
                &summary, mark, at: CGPoint(x: mx, y: top + 24.5), size: 6, color: VignettePalette.dim, weight: .medium)
            VignetteKit.bar(&summary, CGRect(x: mx + 6, y: top + 23.3, width: 10, height: 2.4), VignettePalette.dim)
        }
        let chevron = CGPoint(x: rect.maxX - 18, y: top + VignetteClock.mix(19, 14, open))
        var upward = inside
        upward.opacity *= 1 - ease(open, 0, 0.5)
        PhoneKit.glyph(&upward, "chevron.up", at: chevron, size: 7.5, color: VignettePalette.dim, weight: .bold)
        var downward = inside
        downward.opacity *= ease(open, 0.5, 1)
        PhoneKit.glyph(&downward, "chevron.down", at: chevron, size: 7.5, color: VignettePalette.dim, weight: .bold)

        // Open, the records: each card arriving a beat after the last.
        let cards: [(y: CGFloat, height: CGFloat, name: CGFloat)] = [(34, 50, 40), (90, 40, 26), (136, 38, 30)]
        for (index, card) in cards.enumerated() {
            let shown = ease(open, 0.55 + 0.1 * Double(index), 0.85 + 0.1 * Double(index))
            guard shown > 0.01 else { continue }
            var layer = inside
            layer.opacity *= shown
            let r = CGRect(
                x: rect.minX + 8, y: top + card.y + 6 * CGFloat(1 - shown), width: rect.width - 16, height: card.height)
            PhoneKit.surface(&layer, r, radius: 9)
            PhoneKit.glyph(
                &layer, marks[index], at: CGPoint(x: r.minX + 12, y: r.minY + 10), size: 6.5,
                color: VignettePalette.phosphor)
            VignetteKit.bar(
                &layer, CGRect(x: r.minX + 21, y: r.minY + 8.6, width: card.name, height: 2.8),
                VignettePalette.ink.opacity(0.85))
            switch index {
            case 0: packages(&layer, r)
            case 1: bills(&layer, r)
            default: code(&layer, r)
            }
        }
    }

    /// Two parcels: who sent it, and its way along, as the app's track.
    private static func packages(_ ctx: inout GraphicsContext, _ r: CGRect) {
        let names: [CGFloat] = [46, 36]
        let along = [3, 2]
        for row in 0..<2 {
            let y = r.minY + 23 + CGFloat(row) * 14
            VignetteKit.avatar(&ctx, center: CGPoint(x: r.minX + 12, y: y + 1.3), diameter: 7, tint: row + 1)
            VignetteKit.bar(
                &ctx, CGRect(x: r.minX + 21, y: y, width: names[row], height: 2.6), VignettePalette.ink.opacity(0.75))
            for step in 0..<4 {
                let seg = CGRect(x: r.maxX - 74 + CGFloat(step) * 16, y: y + 0.2, width: 13, height: 2.2)
                VignetteKit.bar(&ctx, seg, step < along[row] ? VignettePalette.phosphor : Color.white.opacity(0.1))
            }
        }
    }

    /// Two bills: who, when, and how much.
    private static func bills(_ ctx: inout GraphicsContext, _ r: CGRect) {
        let names: [CGFloat] = [40, 52]
        for row in 0..<2 {
            let y = r.minY + 21 + CGFloat(row) * 11
            VignetteKit.bar(
                &ctx, CGRect(x: r.minX + 12, y: y, width: names[row], height: 2.6), VignettePalette.ink.opacity(0.75))
            VignetteKit.bar(
                &ctx, CGRect(x: r.minX + 18 + names[row], y: y + 0.2, width: 22, height: 2.2), VignettePalette.dim)
            VignetteKit.bar(
                &ctx, CGRect(x: r.maxX - 12 - 20, y: y, width: 20, height: 2.6), VignettePalette.ink.opacity(0.85))
        }
    }

    /// The newest login code: who it is for, and the code itself as its
    /// figures' boxes.
    private static func code(_ ctx: inout GraphicsContext, _ r: CGRect) {
        let y = r.minY + 26
        VignetteKit.avatar(&ctx, center: CGPoint(x: r.minX + 12, y: y), diameter: 7, tint: 0)
        VignetteKit.bar(
            &ctx, CGRect(x: r.minX + 21, y: y - 1.3, width: 38, height: 2.6), VignettePalette.ink.opacity(0.75))
        for n in 0..<6 {
            let box = CGRect(
                x: r.maxX - 76 + CGFloat(n) * 10.5 + (n > 2 ? 3 : 0), y: y - 5.5, width: 8, height: 11)
            let shape = Path(roundedRect: box, cornerRadius: 2.5, style: .continuous)
            ctx.fill(shape, with: .color(VignettePalette.phosphor.opacity(0.16)))
            ctx.stroke(shape, with: .color(VignettePalette.phosphor.opacity(0.45)), lineWidth: 0.5)
            VignetteKit.bar(
                &ctx, CGRect(x: box.midX - 2, y: box.midY - 1.3, width: 4, height: 2.6),
                VignettePalette.phosphorHot.opacity(0.9))
        }
    }
}

// MARK: - reminders that know the event

/// REMINDERS THAT KNOW THE EVENT: a locked phone, its date and its time, and
/// nothing on it. Then, ahead of the evening's plan by as long as getting
/// there takes, a banner comes up: a calendar tile, a short line, and under
/// it the time and the place, with the ticket beside them. The thumb taps it
/// and the phone opens on the mail the tickets came in, its ticket lit: the
/// event's name, the same time and place, the seats, and the code at the
/// door.
///
/// The seam: the phone locks again, with nothing on it, which is the frame
/// the loop began on.
enum EventReminderScene: VignetteDrawing {
    static let canvas = PhoneKit.canvas
    static let loop: Double = 5.6

    private static let arrive: Double = 0.55
    private static let tap: Double = 1.9
    private static let openFrom: Double = 2.04
    private static let openTo: Double = 2.44
    private static let lockFrom: Double = 4.3
    private static let lockTo: Double = 4.75

    /// The banner on the lock screen, before the thumb.
    static let posterTime: Double = 1.45

    private static let banner = CGRect(x: 12, y: 98, width: 196, height: 48)
    private static let ticket = CGRect(x: 18, y: 76, width: 184, height: 112)

    static func draw(_ ctx: inout GraphicsContext, t: Double) {
        let ease = VignetteClock.ease
        let inApp = ease(t, openFrom, openTo) - ease(t, lockFrom, lockTo)

        if inApp < 1 {
            var lock = ctx
            // One goes before the other comes.
            lock.opacity *= 1 - ease(inApp, 0, 0.6)
            let s = VignetteClock.mix(1, 0.95, inApp)
            lock.translateBy(x: canvas.width / 2, y: canvas.height / 2)
            lock.scaleBy(x: s, y: s)
            lock.translateBy(x: -canvas.width / 2, y: -canvas.height / 2)
            drawLock(&lock, t: t, showing: t < lockFrom)
        }
        if inApp > 0 {
            var app = ctx
            app.opacity *= ease(inApp, 0.4, 1)
            let s = VignetteClock.mix(0.95, 1, inApp)
            app.translateBy(x: banner.midX, y: banner.midY)
            app.scaleBy(x: s, y: s)
            app.translateBy(x: -banner.midX, y: -banner.midY)
            drawReader(&app, t: t)
        }

        let on = ease(t, tap - 0.34, tap - 0.18) * (1 - ease(t, tap + 0.2, tap + 0.36))
        PhoneKit.touch(
            &ctx, at: CGPoint(x: banner.midX - 8, y: banner.midY + 4), presence: on,
            down: VignetteClock.press(t, at: tap))
    }

    /// The lock screen: the date, the time as one tall line, the two corner
    /// buttons, and the banner when there is one.
    private static func drawLock(_ ctx: inout GraphicsContext, t: Double, showing: Bool) {
        let ease = VignetteClock.ease
        VignetteKit.bar(
            &ctx, CGRect(x: canvas.width / 2 - 26, y: 22, width: 52, height: 3), VignettePalette.ink.opacity(0.6))
        VignetteKit.bar(
            &ctx, CGRect(x: canvas.width / 2 - 46, y: 33, width: 92, height: 16), VignettePalette.ink.opacity(0.88))
        for (x, symbol) in [(CGFloat(30), "flashlight.off.fill"), (190, "camera.fill")] {
            let disc = Path(ellipseIn: CGRect(x: x - 11, y: 168, width: 22, height: 22))
            ctx.fill(disc, with: .color(VignettePalette.cap.opacity(0.9)))
            ctx.stroke(disc, with: .color(Color.white.opacity(0.14)), lineWidth: 0.5)
            PhoneKit.glyph(&ctx, symbol, at: CGPoint(x: x, y: 179), size: 7.5, color: VignettePalette.ink.opacity(0.75))
        }
        guard showing else { return }
        let shown = ease(t, arrive, arrive + 0.42)
        guard shown > 0.01 else { return }
        var layer = ctx
        layer.opacity *= shown
        let s = VignetteClock.mix(0.94, 1, shown)
        layer.translateBy(x: banner.midX, y: banner.midY + 14 * CGFloat(1 - shown))
        layer.scaleBy(x: s, y: s)
        layer.translateBy(x: -banner.midX, y: -banner.midY)
        drawBanner(&layer, pressed: VignetteClock.press(t, at: tap))
    }

    /// The heads up: a calendar tile, the app and how long ago, the short
    /// line, the time and the place, and the ticket's thumbnail.
    private static func drawBanner(_ ctx: inout GraphicsContext, pressed: Double) {
        let shape = Path(roundedRect: banner, cornerRadius: 14, style: .continuous)
        var glow = ctx
        glow.addFilter(.shadow(color: Color.black.opacity(0.4), radius: 8, y: 2))
        if pressed > 0 {
            glow.addFilter(.shadow(color: VignettePalette.phosphor.opacity(0.6 * pressed), radius: 6))
        }
        glow.fill(shape, with: .color(VignettePalette.cap))
        ctx.fill(shape, with: .color(Color.white.opacity(0.05 + 0.06 * pressed)))
        ctx.stroke(shape, with: .color(Color.white.opacity(0.16)), lineWidth: 0.5)

        let tile = CGRect(x: banner.minX + 10, y: banner.midY - 12, width: 24, height: 24)
        let tileShape = Path(roundedRect: tile, cornerRadius: 6, style: .continuous)
        ctx.fill(tileShape, with: .color(VignettePalette.card))
        ctx.stroke(tileShape, with: .color(VignettePalette.edge), lineWidth: 0.5)
        VignetteKit.calendarMark(
            &ctx, center: CGPoint(x: tile.midX, y: tile.midY), size: 12, color: VignettePalette.phosphorHot)

        let x = tile.maxX + 9
        VignetteKit.bar(&ctx, CGRect(x: x, y: banner.minY + 10, width: 30, height: 2.2), VignettePalette.dim)
        VignetteKit.bar(
            &ctx, CGRect(x: banner.maxX - 44 - 14, y: banner.minY + 10, width: 14, height: 2.2), VignettePalette.dim)
        VignetteKit.bar(
            &ctx, CGRect(x: x, y: banner.minY + 19, width: 84, height: 3.4), VignettePalette.ink.opacity(0.92))
        timeAndPlace(&ctx, at: CGPoint(x: x, y: banner.minY + 31))

        // The ticket, one tap away.
        let thumb = CGRect(x: banner.maxX - 38, y: banner.midY - 13, width: 28, height: 26)
        let thumbShape = Path(roundedRect: thumb, cornerRadius: 6, style: .continuous)
        ctx.fill(thumbShape, with: .color(VignettePalette.phosphor.opacity(0.16)))
        ctx.stroke(thumbShape, with: .color(VignettePalette.phosphor.opacity(0.5)), lineWidth: 0.5)
        PhoneKit.glyph(
            &ctx, "ticket", at: CGPoint(x: thumb.midX, y: thumb.midY), size: 9, color: VignettePalette.phosphorHot)
    }

    /// The time, a dot, the place: the same words on the banner and on the
    /// ticket.
    private static func timeAndPlace(_ ctx: inout GraphicsContext, at origin: CGPoint) {
        VignetteKit.bar(&ctx, CGRect(x: origin.x, y: origin.y, width: 14, height: 2.6), VignettePalette.phosphorHot)
        ctx.fill(
            Path(ellipseIn: CGRect(x: origin.x + 18, y: origin.y + 0.4, width: 1.8, height: 1.8)),
            with: .color(VignettePalette.dim))
        VignetteKit.bar(
            &ctx, CGRect(x: origin.x + 24, y: origin.y, width: 46, height: 2.6), VignettePalette.ink.opacity(0.7))
    }

    /// The ticket email: who sent it, its subject, and the ticket.
    private static func drawReader(_ ctx: inout GraphicsContext, t: Double) {
        let ease = VignetteClock.ease
        let back = Path(ellipseIn: CGRect(x: 15, y: 7, width: 20, height: 20))
        ctx.fill(back, with: .color(VignettePalette.cap))
        ctx.stroke(back, with: .color(Color.white.opacity(0.14)), lineWidth: 0.5)
        PhoneKit.glyph(
            &ctx, "chevron.left", at: CGPoint(x: 25, y: 17), size: 8.5, color: VignettePalette.ink.opacity(0.7),
            weight: .bold)

        VignetteKit.avatar(&ctx, center: CGPoint(x: 26, y: 43), diameter: 14, tint: 2)
        VignetteKit.bar(&ctx, CGRect(x: 38, y: 38.5, width: 52, height: 3.2), VignettePalette.ink.opacity(0.85))
        VignetteKit.bar(&ctx, CGRect(x: 38, y: 45.5, width: 30, height: 2.4), VignettePalette.dim)
        VignetteKit.bar(&ctx, CGRect(x: 182, y: 39, width: 20, height: 2.4), VignettePalette.dim)
        VignetteKit.bar(&ctx, CGRect(x: 18, y: 60, width: 128, height: 4.2), VignettePalette.ink.opacity(0.9))

        // The ticket: two notches where it tears, lit as it is landed on.
        let lit = ease(t, openTo - 0.1, openTo + 0.2) * (1 - ease(t, openTo + 1.1, openTo + 1.6))
        let tear = ticket.minY + 72
        let notches = Path(ellipseIn: CGRect(x: ticket.minX - 6, y: tear - 6, width: 12, height: 12))
            .union(Path(ellipseIn: CGRect(x: ticket.maxX - 6, y: tear - 6, width: 12, height: 12)))
        let body = Path(roundedRect: ticket, cornerRadius: 10, style: .continuous).subtracting(notches)
        var glow = ctx
        if lit > 0 { glow.addFilter(.shadow(color: VignettePalette.phosphor.opacity(0.55 * lit), radius: 7)) }
        glow.fill(body, with: .color(VignettePalette.card))
        ctx.fill(body, with: .color(VignettePalette.faint))
        ctx.stroke(body, with: .color(VignettePalette.edge), lineWidth: 0.5)
        if lit > 0 {
            ctx.stroke(body, with: .color(VignettePalette.phosphor.opacity(0.85 * lit)), lineWidth: 0.7)
        }

        let x = ticket.minX + 14
        VignetteKit.calendarMark(
            &ctx, center: CGPoint(x: x + 4, y: ticket.minY + 16), size: 8, color: VignettePalette.phosphorHot)
        VignetteKit.bar(
            &ctx, CGRect(x: x + 14, y: ticket.minY + 14.2, width: 92, height: 4), VignettePalette.ink.opacity(0.92))
        timeAndPlace(&ctx, at: CGPoint(x: x + 14, y: ticket.minY + 26))
        // The seats: three small labels over what they say.
        for (index, width) in [CGFloat(16), 12, 14].enumerated() {
            let cx = x + CGFloat(index) * 52
            VignetteKit.bar(&ctx, CGRect(x: cx, y: ticket.minY + 44, width: 18, height: 2), VignettePalette.dim)
            VignetteKit.bar(
                &ctx, CGRect(x: cx, y: ticket.minY + 51, width: width, height: 3.4), VignettePalette.ink.opacity(0.85))
        }
        // Where it tears, and the code at the door.
        var perforation = Path()
        perforation.move(to: CGPoint(x: ticket.minX + 10, y: tear))
        perforation.addLine(to: CGPoint(x: ticket.maxX - 10, y: tear))
        ctx.stroke(
            perforation, with: .color(Color.white.opacity(0.16)), style: StrokeStyle(lineWidth: 0.6, dash: [2.5, 2.5]))
        let widths: [CGFloat] = [1.2, 0.6, 1.8, 0.6, 0.9, 2.2, 0.6, 1.2, 0.6, 1.8, 0.9, 0.6, 2.2, 0.9, 0.6, 1.2]
        var bx = ticket.minX + 46
        var n = 0
        while bx < ticket.maxX - 46 {
            let w = widths[n % widths.count]
            ctx.fill(
                Path(CGRect(x: bx, y: tear + 11, width: w, height: 18)), with: .color(VignettePalette.ink.opacity(0.78)))
            bx += w + 1.3
            n += 1
        }
    }
}
