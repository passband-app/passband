// THE SCENES the What's New walk plays, one per `ReleaseVignette` key in the
// release table. Each is held to the language at the top of VignetteStage.swift:
// a pure function of time on a 168 x 96 night screen, bars for text, one actor
// colour, a loop that ends on the frame it began on.
//
// ADDING ONE: a case on `ReleaseVignette` (Lib/ReleaseNotes.swift), a scene
// here, and its line in `scene` below. The compiler insists on the last.

import SwiftUI

extension ReleaseVignette {
    var scene: any VignetteDrawing.Type {
        switch self {
        case .mailList: MailListScene.self
        case .flow: FlowScene.self
        case .unsubscribe: UnsubscribeScene.self
        case .clearStack: ClearStackScene.self
        }
    }
}

// MARK: - all mail

/// ALL MAIL AS A LIST: filter pills, day headings, and rows of who / what /
/// when, the mail triage picked set heavier. The Important pill lights and the
/// rest of the list steps out, the picked rows closing up under their days;
/// then All brings everything back.
enum MailListScene: VignetteDrawing {
    /// The whole list, at rest: the page is about the list, not the filter.
    static let posterTime: Double = 0.5

    private struct Row {
        let important: Bool
        let tint: Int
        let sender: CGFloat
        let subject: CGFloat
        /// Top edge with everything showing, and with only the picked mail.
        let y: CGFloat
        let yFiltered: CGFloat
    }

    private static let rows: [Row] = [
        Row(important: true, tint: 0, sender: 20, subject: 46, y: 30, yFiltered: 30),
        Row(important: false, tint: 1, sender: 15, subject: 34, y: 40, yFiltered: 40),
        Row(important: true, tint: 2, sender: 22, subject: 40, y: 50, yFiltered: 40),
        Row(important: true, tint: 1, sender: 17, subject: 50, y: 68, yFiltered: 58),
        Row(important: false, tint: 0, sender: 20, subject: 30, y: 78, yFiltered: 78),
    ]
    /// The two day headings' top edges, in both arrangements.
    private static let headings: [(y: CGFloat, yFiltered: CGFloat, width: CGFloat)] = [
        (23, 23, 16), (61, 51, 24),
    ]
    private static let pills: [(x: CGFloat, width: CGFloat)] = [(12, 16), (32, 28), (64, 28), (96, 32)]

    private static let filterOn: Double = 1.0
    private static let filterOff: Double = 3.1

    static func draw(_ ctx: inout GraphicsContext, t: Double) {
        let ease = VignetteClock.ease
        // Going in, the unpicked rows fade and THEN the rest close up; coming
        // back, the rows part first and the others return to the gaps.
        let selected = ease(t, filterOn, filterOn + 0.12) - ease(t, filterOff, filterOff + 0.12)
        let hidden = ease(t, filterOn + 0.02, filterOn + 0.32) - ease(t, filterOff + 0.28, filterOff + 0.6)
        let closed = ease(t, filterOn + 0.22, filterOn + 0.62) - ease(t, filterOff, filterOff + 0.38)

        for (index, pill) in pills.enumerated() {
            let on = index == 0 ? 1 - selected : index == 1 ? selected : 0
            let tap = index == 0 ? VignetteClock.press(t, at: filterOff) : index == 1 ? VignetteClock.press(t, at: filterOn) : 0
            let rect = CGRect(x: pill.x, y: 8, width: pill.width, height: 9)
            let shape = Path(roundedRect: rect, cornerRadius: 4.5)
            var layer = ctx
            if tap > 0 {
                layer.addFilter(.shadow(color: VignettePalette.phosphor.opacity(0.8 * tap), radius: 5))
            }
            layer.fill(shape, with: .color(VignettePalette.faint))
            layer.fill(shape, with: .color(VignettePalette.phosphor.opacity(0.9 * on)))
            ctx.stroke(shape, with: .color(VignettePalette.edge.opacity(1 - 0.7 * on)), lineWidth: 0.5)
            VignetteKit.bar(
                &ctx, CGRect(x: rect.minX + 4.5, y: rect.midY - 1.25, width: rect.width - 9, height: 2.5),
                on > 0.5 ? Color.white.opacity(0.95) : VignettePalette.dim)
        }

        for heading in headings {
            let y = VignetteClock.mix(heading.y, heading.yFiltered, closed)
            VignetteKit.bar(
                &ctx, CGRect(x: 12, y: y, width: heading.width, height: 3), VignettePalette.ink.opacity(0.5))
            ctx.fill(
                Path(CGRect(x: 16 + heading.width, y: y + 1.25, width: 140 - heading.width, height: 0.5)),
                with: .color(VignettePalette.edge))
        }

        for row in rows {
            let y = VignetteClock.mix(row.y, row.yFiltered, closed)
            var layer = ctx
            layer.opacity = row.important ? 1 : 1 - hidden
            // The importance meter in the gutter: lit for what triage picked.
            VignetteKit.bar(
                &layer, CGRect(x: 12, y: y + 1, width: 2, height: 6),
                row.important ? VignettePalette.phosphor.opacity(0.9) : Color.white.opacity(0.1))
            layer.fill(
                Path(ellipseIn: CGRect(x: 18, y: y + 1, width: 6, height: 6)),
                with: .color(VignettePalette.avatar[row.tint].opacity(0.85)))
            let weight = row.important ? 0.85 : 0.38
            VignetteKit.bar(
                &layer, CGRect(x: 28, y: y + 2.5, width: row.sender, height: 3), VignettePalette.ink.opacity(weight))
            VignetteKit.bar(
                &layer, CGRect(x: 54, y: y + 2.5, width: row.subject, height: 3), VignettePalette.ink.opacity(weight))
            // The one-liner after the subject, where a mail client prints the
            // opening of the body.
            let lineX = 54 + row.subject + 4
            VignetteKit.bar(
                &layer, CGRect(x: lineX, y: y + 2.5, width: 138 - lineX, height: 3),
                VignettePalette.dim.opacity(0.7))
            VignetteKit.bar(&layer, CGRect(x: 146, y: y + 2.75, width: 10, height: 2.5), VignettePalette.dim)
        }
    }
}

// MARK: - flow

/// FLOW MODE: Space starts the walk (the lane lights and the email in front is
/// taken up), then mail goes past one at a time. Two `⇧E`s each finish the
/// email in front (a check draws on, the card lifts away through the top, the
/// next one steps up), then a `⇧S` sets one aside to the left, unfinished and
/// unjudged. Three moves, three card designs, so the deck is back where it
/// began.
enum FlowScene: VignetteDrawing {
    private enum Kind { case done, skip }
    private static let start: Double = 0.4
    private static let events: [(at: Double, kind: Kind)] = [
        (1.3, .done), (2.35, .done), (3.4, .skip),
    ]
    /// An event's whole span: press, mark, departure, the deck stepping up.
    private static let span: Double = 0.84

    /// `⇧E` down and the check just drawn, the card still in place.
    static let posterTime: Double = 1.46

    private static let deck = CGPoint(x: 14, y: 26)

    static func draw(_ ctx: inout GraphicsContext, t: Double) {
        let walking = VignetteClock.ease(t, start + 0.04, start + 0.3) - VignetteClock.ease(t, 4.3, 4.62)

        // The lane: a short rule over the deck, For Your Eyes' header, lit
        // while the walk lasts.
        VignetteKit.bar(
            &ctx, CGRect(x: deck.x, y: 13, width: 24, height: 3),
            VignettePalette.phosphor.opacity(VignetteClock.mix(0.4, 0.95, walking)))
        VignetteKit.bar(&ctx, CGRect(x: deck.x + 28, y: 13, width: 12, height: 3), VignettePalette.dim)

        let finished = events.filter { t >= $0.at + span }.count
        let active = events.first { t >= $0.at && t < $0.at + span }
        let local = active.map { t - $0.at } ?? 0
        let depart = active == nil ? 0 : VignetteClock.ease(local, 0.30, 0.62)
        let promote = active == nil ? 0 : VignetteClock.ease(local, 0.42, 0.82)

        VignetteKit.deckBehind(&ctx, origin: deck, first: finished, promote: promote)

        // The card in front, leaving the way its key sends it.
        let kind = active?.kind ?? .done
        let front = CGPoint(
            x: deck.x - (kind == .skip ? 26 * depart : 0),
            y: deck.y - (kind == .done ? 24 * depart : 0))
        VignetteKit.mail(&ctx, origin: front, design: finished % 3, opacity: 1 - depart, lit: walking)

        // Done draws its check on the card before the card goes.
        if kind == .done, active != nil {
            VignetteKit.check(
                &ctx, center: CGPoint(x: front.x + 84, y: front.y + 15),
                progress: VignetteClock.ease(local, 0.03, 0.16), opacity: 1 - depart)
        }

        // The keys, right of the deck, in the order they are used.
        let doneDown = events.filter { $0.kind == .done }.map { VignetteClock.press(t, at: $0.at) }.max() ?? 0
        let skipDown = events.filter { $0.kind == .skip }.map { VignetteClock.press(t, at: $0.at) }.max() ?? 0
        VignetteKit.keycap(
            &ctx, "space", origin: CGPoint(x: 122, y: 11), width: 38, pressed: VignetteClock.press(t, at: start))
        VignetteKit.keycap(&ctx, "⇧E", origin: CGPoint(x: 122, y: 37), width: 30, pressed: doneDown)
        VignetteKit.keycap(&ctx, "⇧S", origin: CGPoint(x: 122, y: 63), width: 30, pressed: skipDown)
    }
}

// MARK: - unsubscribe

/// UNSUBSCRIBE: `u` on the email in front, and Passband goes and does it. A
/// chip arrives in the corner carrying the sender's mark, a ring closes round
/// it while the daemon works, a check lands, and that sender's mail is gone.
/// Three senders, three presses, so the deck is back where it began.
enum UnsubscribeScene: VignetteDrawing {
    private static let events: [Double] = [0.45, 1.85, 3.25]
    private static let span: Double = 1.4

    /// The key just let go, the chip in and its ring half closed.
    static let posterTime: Double = 0.95

    private static let deck = CGPoint(x: 14, y: 22)
    private static let chip = CGRect(x: 112, y: 60, width: 48, height: 20)

    static func draw(_ ctx: inout GraphicsContext, t: Double) {
        let ease = VignetteClock.ease
        let finished = events.filter { t >= $0 + span }.count
        let active = events.first { t >= $0 && t < $0 + span }
        let local = active.map { t - $0 } ?? 0
        let on = active != nil
        let depart = on ? ease(local, 0.8, 1.08) : 0
        let promote = on ? ease(local, 0.92, 1.32) : 0

        VignetteKit.deckBehind(&ctx, origin: deck, first: finished, promote: promote)

        // The card in front goes where it stands: nothing files it anywhere,
        // it just stops being mail you get.
        VignetteKit.mail(
            &ctx, origin: deck, design: finished % 3, opacity: 1 - depart,
            scale: VignetteClock.mix(1, 0.95, depart))

        VignetteKit.keycap(
            &ctx, "U", origin: CGPoint(x: 130, y: 20), pressed: events.map { VignetteClock.press(t, at: $0) }.max() ?? 0)

        // The tray's chip: the sender's mark, the work, the result.
        guard on else { return }
        let arrive = ease(local, 0.06, 0.26)
        let leave = ease(local, 1.1, 1.36)
        let working = ease(local, 0.24, 0.74)
        let done = ease(local, 0.74, 0.9)

        var layer = ctx
        layer.opacity = arrive * (1 - leave)
        layer.translateBy(x: 8 * CGFloat(1 - arrive), y: 0)
        let shape = Path(roundedRect: chip, cornerRadius: chip.height / 2)
        layer.fill(shape, with: .color(VignettePalette.cap))
        layer.stroke(shape, with: .color(VignettePalette.edge), lineWidth: 0.6)
        let mark = CGPoint(x: chip.minX + 11, y: chip.midY)
        layer.fill(
            Path(ellipseIn: CGRect(x: mark.x - 3.5, y: mark.y - 3.5, width: 7, height: 7)),
            with: .color(VignettePalette.avatar[finished % 3].opacity(0.85)))
        VignetteKit.ring(
            &layer, center: mark, radius: 6, progress: working,
            color: done > 0.5 ? VignettePalette.phosphorHot : VignettePalette.phosphor, lineWidth: 1.3)
        VignetteKit.bar(
            &layer, CGRect(x: chip.minX + 22, y: chip.midY - 1.5, width: 18, height: 3),
            VignettePalette.dim.opacity(1 - done))
        VignetteKit.check(&layer, center: CGPoint(x: chip.minX + 31, y: chip.midY), size: 4, progress: done)
    }
}

// MARK: - clear a stack

/// HOLD E ON A READING STACK: the key goes down and stays down, a ring closes
/// round it at the app's own pace, and when it closes every email in the stack
/// is finished at once. One sender's mail, so one tint all the way down. A new
/// stack then rises into the same place.
enum ClearStackScene: VignetteDrawing {
    private static let down: Double = 0.7
    /// The app waits 180ms before the ring shows and closes it in 750ms.
    private static let ringFrom: Double = 0.88
    private static let ringTo: Double = 1.63
    private static let up: Double = 1.75
    private static let returnFrom: Double = 2.8
    private static let returnTo: Double = 3.5

    /// Held, with the ring a little past half.
    static let posterTime: Double = 1.3

    private static let stack = CGPoint(x: 14, y: 24)
    private static let poseY: [CGFloat] = [0, 8, 15, 21]
    private static let poseScale: [CGFloat] = [1, 0.94, 0.88, 0.82]
    private static let poseOpacity: [Double] = [1, 0.7, 0.45, 0.25]
    /// Reading's own colour here: the paper tint.
    private static let tint = 2

    static func draw(_ ctx: inout GraphicsContext, t: Double) {
        let ease = VignetteClock.ease

        // The stack's label: whose mail this is.
        VignetteKit.bar(
            &ctx, CGRect(x: stack.x, y: 13, width: 24, height: 3),
            VignettePalette.avatar[tint].opacity(0.75))
        VignetteKit.bar(&ctx, CGRect(x: stack.x + 28, y: 13, width: 12, height: 3), VignettePalette.dim)

        let back = ease(t, returnFrom, returnTo)
        // Back to front. Going, the cards lift away together, the front one a
        // breath ahead; coming back, the whole stack rises as one.
        for depth in stride(from: 3, through: 0, by: -1) {
            let lag = 0.05 * Double(depth)
            let gone = ease(t, up - 0.03 + lag, up + 0.4 + lag)
            let returning = t >= returnFrom
            let opacity = poseOpacity[depth] * (returning ? back : 1 - gone)
            let lift: CGFloat = returning ? 8 * CGFloat(1 - back) : -14 * CGFloat(gone)
            let origin = CGPoint(x: stack.x, y: stack.y + poseY[depth] + lift)
            VignetteKit.mail(
                &ctx, origin: origin, design: depth, tint: tint, opacity: opacity, scale: poseScale[depth],
                blank: depth > 0)
            if depth == 0 {
                VignetteKit.check(
                    &ctx, center: CGPoint(x: origin.x + 84, y: origin.y + 15),
                    progress: ease(t, ringTo - 0.02, ringTo + 0.12), opacity: returning ? 0 : 1 - gone)
            }
        }

        // The key, held, and the ring that makes a hold a decision.
        let key = CGPoint(x: 128, y: 26)
        let center = CGPoint(x: key.x + 11, y: key.y + 11)
        let closed = ease(t, ringTo - 0.04, ringTo + 0.04)
        let ringOut = ease(t, up + 0.1, up + 0.4)
        VignetteKit.ring(
            &ctx, center: center, radius: 17, progress: 1, color: VignettePalette.edge, lineWidth: 1.2,
            opacity: ease(t, ringFrom - 0.08, ringFrom) * (1 - ringOut))
        var glow = ctx
        if closed > 0 {
            glow.addFilter(.shadow(color: VignettePalette.phosphor.opacity(0.9 * closed), radius: 5))
        }
        VignetteKit.ring(
            &glow, center: center, radius: 17, progress: VignetteClock.ramp(t, ringFrom, ringTo),
            color: closed > 0.5 ? VignettePalette.phosphorHot : VignettePalette.phosphor, opacity: 1 - ringOut)
        VignetteKit.keycap(&ctx, "E", origin: key, pressed: VignetteClock.hold(t, from: down, to: up))
    }
}
