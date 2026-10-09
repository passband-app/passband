// THE SCENES the What's New walk plays, one per `ReleaseVignette` key in the
// release table. Each is held to the language at the top of VignetteStage.swift:
// a pure function of time on a 240 x 120 night screen, bars for text, one actor
// colour, a loop that ends on the frame it began on.
//
// Each one TEACHES rather than decorates: somebody who has never used the
// feature should be able to watch one loop and do it. So each shows the thing
// the key acts on, the key, and what changes, in that order, and each ends
// where it began because the next thing arrives as the last one leaves.
//
// ADDING ONE: a case on `ReleaseVignette` (Lib/ReleaseNotes.swift), a scene
// here, and its line in `scene` below. The compiler insists on the last.

import SwiftUI

extension ReleaseVignette {
    /// The drawn scene, or nil for a page whose picture is not one of these
    /// (the agent door plays the agent ad's conversation; see WhatsNewCard).
    var scene: (any VignetteDrawing.Type)? {
        switch self {
        case .mailList: MailListScene.self
        case .flow: FlowScene.self
        case .unsubscribe: UnsubscribeScene.self
        case .clearStack: ClearStackScene.self
        case .smartBlock: SmartBlockScene.self
        case .loginCode: LoginCodeScene.self
        case .agentWrite: AgentWriteScene.self
        case .smartUnsubscribe: SmartUnsubscribeScene.self
        case .agentSession: nil
        }
    }
}

extension VignetteKit {
    /// Clips what follows to `rect`, its bottom `fade` units dissolving into
    /// the ground, so a list reads as running on past the screen rather than
    /// as cut off.
    static func clipFading(_ ctx: inout GraphicsContext, _ rect: CGRect, fade: CGFloat) {
        ctx.clipToLayer { mask in
            mask.fill(
                Path(rect),
                with: .linearGradient(
                    Gradient(stops: [
                        .init(color: .black, location: 0),
                        .init(color: .black, location: 1 - fade / rect.height),
                        .init(color: .clear, location: 1),
                    ]),
                    startPoint: CGPoint(x: 0, y: rect.minY), endPoint: CGPoint(x: 0, y: rect.maxY)))
        }
    }
}

// MARK: - all mail

/// THE ALL MAIL OVERHAUL: the filter row, day headings, and rows of who /
/// what / when, the way thirty years of mail clients taught people to read
/// one, with finished mail kept, dimmed and checked. The page sells the LIST,
/// so the poster is the whole of it at rest and every part is on screen. The
/// list scrolls back one day (the heading of the day before rises into view,
/// the rows under it carrying dates instead of times), then the Important
/// lens narrows it to what triage picked, then All brings everything back.
///
/// The scroll is exactly one day long and every day is drawn alike, so the
/// list it lands on is the list it left: the seam is a page that keeps
/// scrolling back through your mail, which is the point.
enum MailListScene: VignetteDrawing {
    /// The whole list at rest, All lit: the page is about the list.
    static let posterTime: Double = 0.3

    private struct Row {
        let important: Bool
        let done: Bool
        let tint: Int
        let sender: CGFloat
        let subject: CGFloat
        /// Where the one-liner after the subject stops, short of the date.
        let gistEnd: CGFloat
    }

    /// One day's mail. Picked mail is set heavier; finished mail stays,
    /// dimmed and checked, because archived is not gone.
    private static let day: [Row] = [
        Row(important: true, done: false, tint: 0, sender: 26, subject: 42, gistEnd: 178),
        Row(important: false, done: false, tint: 1, sender: 18, subject: 30, gistEnd: 150),
        Row(important: true, done: false, tint: 2, sender: 30, subject: 56, gistEnd: 190),
        Row(important: false, done: true, tint: 0, sender: 22, subject: 36, gistEnd: 164),
        Row(important: false, done: false, tint: 1, sender: 24, subject: 26, gistEnd: 172),
    ]
    private static let headingHeight: CGFloat = 15
    private static let rowHeight: CGFloat = 11.5
    private static let listTop: CGFloat = 26

    /// The lenses, left to right: All, Important, Deadlines, Attachments,
    /// Reminders. Bars for their names.
    private static let pills: [CGFloat] = [18, 36, 33, 38, 34]

    private static let scrollFrom: Double = 0.55
    private static let scrollTo: Double = 1.55
    private static let filterOn: Double = 2.0
    private static let filterOff: Double = 3.45

    static func draw(_ ctx: inout GraphicsContext, t: Double) {
        let ease = VignetteClock.ease
        // Going in, the unpicked rows fade and THEN the rest close up; coming
        // back, the rows part first and the others return to the gaps.
        let selected = ease(t, filterOn, filterOn + 0.12) - ease(t, filterOff, filterOff + 0.12)
        let hidden = ease(t, filterOn + 0.04, filterOn + 0.3) - ease(t, filterOff + 0.26, filterOff + 0.56)
        let closed = ease(t, filterOn + 0.22, filterOn + 0.62) - ease(t, filterOff, filterOff + 0.38)

        drawFilterBar(&ctx, t: t, selected: selected)

        // The heights a day takes with everything showing and with only the
        // picked mail, and where each row sits in either.
        let kept = day.filter { $0.important }.count
        let dayHeight = headingHeight + rowHeight * VignetteClock.mix(CGFloat(day.count), CGFloat(kept), closed)
        let scrolled = ease(t, scrollFrom, scrollTo) * dayHeight

        var list = ctx
        VignetteKit.clipFading(&list, CGRect(x: 0, y: listTop, width: 240, height: 120 - listTop), fade: 22)
        for dayIndex in 0..<3 {
            let top = listTop + CGFloat(dayIndex) * dayHeight - scrolled
            guard top < 120, top + dayHeight > listTop - 4 else { continue }
            // The day heading: a short label on the rows' text edge, the way
            // the app sets "TODAY" and "YESTERDAY", with no rule under it.
            // The day that scrolls in lights its heading as it passes, the
            // eye's cue that this list is cut into days now.
            let cue = dayIndex == 1
                ? VignetteClock.ease(t, scrollFrom + 0.3, scrollTo - 0.1) * (1 - VignetteClock.ease(t, scrollTo + 0.15, scrollTo + 0.5))
                : 0
            let headingColor = cue > 0
                ? VignettePalette.phosphorHot.opacity(VignetteClock.mix(0.5, 0.95, cue))
                : VignettePalette.ink.opacity(0.5)
            var heading = list
            if cue > 0 { heading.addFilter(.shadow(color: VignettePalette.phosphor.opacity(0.8 * cue), radius: 3)) }
            VignetteKit.bar(&heading, CGRect(x: 26, y: top + 6, width: 24, height: 2.6), headingColor)
            var shown: CGFloat = 0
            for row in day {
                let y = top + headingHeight + rowHeight * shown
                let fade = row.important ? 1 : 1 - hidden
                drawRow(&list, row, y: y, older: dayIndex > 0, opacity: fade)
                shown += row.important ? 1 : CGFloat(1 - closed)
            }
        }
    }

    private static func drawFilterBar(_ ctx: inout GraphicsContext, t: Double, selected: Double) {
        var x: CGFloat = 26
        for (index, width) in pills.enumerated() {
            let on = index == 0 ? 1 - selected : index == 1 ? selected : 0
            let tap =
                index == 0
                ? VignetteClock.press(t, at: filterOff) : index == 1 ? VignetteClock.press(t, at: filterOn) : 0
            let rect = CGRect(x: x, y: 11, width: width, height: 10)
            let shape = Path(roundedRect: rect, cornerRadius: 5)
            var layer = ctx
            if tap > 0 {
                layer.addFilter(.shadow(color: VignettePalette.phosphor.opacity(0.85 * tap), radius: 4))
            }
            layer.fill(shape, with: .color(VignettePalette.faint))
            layer.fill(shape, with: .color(VignettePalette.phosphor.opacity(0.92 * on)))
            ctx.stroke(shape, with: .color(VignettePalette.edge.opacity(1 - 0.7 * on)), lineWidth: 0.5)
            VignetteKit.bar(
                &ctx, CGRect(x: rect.minX + 5, y: rect.midY - 1.1, width: rect.width - 10, height: 2.2),
                on > 0.5 ? Color.white.opacity(0.95) : VignettePalette.dim)
            x += width + 4
        }
        // The zone picker on the right edge: there, and not what this is about.
        let picker = CGRect(x: 204, y: 11, width: 26, height: 10)
        ctx.stroke(Path(roundedRect: picker, cornerRadius: 5), with: .color(VignettePalette.edge), lineWidth: 0.5)
        VignetteKit.bar(&ctx, CGRect(x: picker.minX + 5, y: picker.midY - 1.1, width: 16, height: 2.2), VignettePalette.dim)
    }

    /// One row: gutter, avatar, sender, subject then the one-liner, date.
    private static func drawRow(_ ctx: inout GraphicsContext, _ row: Row, y: CGFloat, older: Bool, opacity: Double) {
        guard opacity > 0.01 else { return }
        var layer = ctx
        layer.opacity *= opacity
        let mid = y + rowHeight / 2
        let weight = row.done ? 0.26 : row.important ? 0.88 : 0.5
        // The gutter: a check on finished mail, the importance meter on the
        // rest, lit for what triage picked.
        if row.done {
            VignetteKit.check(&layer, center: CGPoint(x: 15, y: mid), size: 2.4, progress: 1, opacity: 0.6)
        } else {
            for i in 0..<3 {
                let lit = row.important || i == 0
                layer.fill(
                    Path(roundedRect: CGRect(x: 11 + CGFloat(i) * 2.6, y: mid - 2.2, width: 1.6, height: 4.4), cornerRadius: 0.8),
                    with: .color(lit ? VignettePalette.phosphor.opacity(row.important ? 0.95 : 0.45) : Color.white.opacity(0.1)))
            }
        }
        VignetteKit.avatar(&layer, center: CGPoint(x: 29, y: mid), diameter: 6, tint: row.tint, opacity: row.done ? 0.4 : 0.9)
        let barY = mid - 1.4
        VignetteKit.bar(&layer, CGRect(x: 36, y: barY, width: row.sender, height: 2.8), VignettePalette.ink.opacity(weight))
        VignetteKit.bar(&layer, CGRect(x: 74, y: barY, width: row.subject, height: 2.8), VignettePalette.ink.opacity(weight))
        let gistX = 74 + row.subject + 4
        VignetteKit.bar(
            &layer, CGRect(x: gistX, y: barY + 0.2, width: row.gistEnd - gistX, height: 2.4),
            VignettePalette.dim.opacity(row.done ? 0.4 : 0.75))
        // Today's mail carries a time, the days before a date: a touch wider.
        let date: CGFloat = older ? 17 : 11
        VignetteKit.bar(
            &layer, CGRect(x: 230 - date, y: barY + 0.2, width: date, height: 2.4),
            VignettePalette.dim.opacity(row.important ? 1 : 0.7))
    }
}

// MARK: - flow

/// FLOW MODE: For Your Eyes on the left, the reader on the right. Space opens
/// the first email and the lane lights; `⇧E` finishes the one open (a check
/// on the page, the page lifts away, its row leaves the lane) and the next
/// one opens in its place; `⇧S` slides the open one aside, unfinished, its
/// row left where it was. Then the walk ends and the reader closes.
///
/// The lane alternates two senders, so after two rows have left and one was
/// skipped, the lane the walk leaves is the lane it began on.
enum FlowScene: VignetteDrawing {
    private enum Kind { case done, skip }
    private static let start: Double = 0.45
    private static let events: [(at: Double, kind: Kind)] = [
        (1.35, .done), (2.3, .done), (3.25, .skip),
    ]
    private static let end: Double = 4.1

    /// `⇧E` down and its check drawn on the page, the lane lit.
    static let posterTime: Double = 1.5

    private static let lane = CGRect(x: 10, y: 26, width: 70, height: 84)
    private static let pitch: CGFloat = 17
    private static let page = CGRect(x: 92, y: 12, width: 138, height: 74)

    /// Which lane row is open at `t`, and how open (0...1), for the row's
    /// light and the page's presence.
    private static func openness(_ index: Int, _ t: Double) -> Double {
        let ease = VignetteClock.ease
        let opens: [Double] = [start + 0.05, 1.35 + 0.42, 2.3 + 0.42, 3.25 + 0.34]
        let closes: [Double] = [1.35 + 0.26, 2.3 + 0.26, 3.25 + 0.16, end]
        guard index < opens.count else { return 0 }
        return ease(t, opens[index], opens[index] + 0.28) - ease(t, closes[index], closes[index] + 0.28)
    }

    static func draw(_ ctx: inout GraphicsContext, t: Double) {
        let ease = VignetteClock.ease
        let walking = ease(t, start + 0.04, start + 0.3) - ease(t, end, end + 0.3)

        // The lane's header: For Your Eyes, lit while the walk lasts.
        VignetteKit.bar(
            &ctx, CGRect(x: lane.minX + 1, y: 13, width: 30, height: 3),
            VignettePalette.phosphor.opacity(VignetteClock.mix(0.45, 1, walking)))
        VignetteKit.bar(&ctx, CGRect(x: lane.minX + 35, y: 13.25, width: 12, height: 2.5), VignettePalette.dim)

        drawLane(&ctx, t: t)
        drawReader(&ctx, t: t)

        // The keys, under the reader, in the order they are used.
        let doneDown = events.filter { $0.kind == .done }.map { VignetteClock.press(t, at: $0.at) }.max() ?? 0
        let skipDown = events.filter { $0.kind == .skip }.map { VignetteClock.press(t, at: $0.at) }.max() ?? 0
        VignetteKit.keycap(&ctx, "space", origin: CGPoint(x: 140, y: 94), width: 36, pressed: VignetteClock.press(t, at: start))
        VignetteKit.keycap(&ctx, "⇧E", origin: CGPoint(x: 181, y: 94), width: 22, pressed: doneDown)
        VignetteKit.keycap(&ctx, "⇧S", origin: CGPoint(x: 208, y: 94), width: 22, pressed: skipDown)
    }

    private static func drawLane(_ ctx: inout GraphicsContext, t: Double) {
        let ease = VignetteClock.ease
        var layer = ctx
        VignetteKit.clipFading(&layer, CGRect(x: 0, y: lane.minY - 2, width: lane.maxX + 4, height: lane.height + 2), fade: 18)
        var y = lane.minY
        for index in 0..<7 {
            // Rows 0 and 1 are finished: a check, then the row leaves and the
            // ones below close up.
            let doneAt: Double? = index == 0 ? events[0].at : index == 1 ? events[1].at : nil
            let check = doneAt.map { ease(t, $0 + 0.06, $0 + 0.2) } ?? 0
            let leave = doneAt.map { ease(t, $0 + 0.34, $0 + 0.64) } ?? 0
            let rect = CGRect(x: lane.minX, y: y, width: lane.width, height: 14)
            if leave < 1 {
                var row = layer
                row.opacity *= 1 - leave
                let open = openness(index, t)
                let shape = Path(roundedRect: rect, cornerRadius: 4, style: .continuous)
                row.fill(shape, with: .color(VignettePalette.card))
                row.fill(shape, with: .color(VignettePalette.phosphor.opacity(0.1 * open)))
                row.stroke(shape, with: .color(VignettePalette.edge), lineWidth: 0.5)
                if open > 0 {
                    row.stroke(shape, with: .color(VignettePalette.phosphor.opacity(0.9 * open)), lineWidth: 0.7)
                }
                let tint = index % 2 == 0 ? 0 : 1
                VignetteKit.avatar(&row, center: CGPoint(x: rect.minX + 8, y: rect.midY), diameter: 6, tint: tint)
                VignetteKit.bar(
                    &row, CGRect(x: rect.minX + 15, y: rect.minY + 4, width: index % 2 == 0 ? 34 : 26, height: 2.4),
                    VignettePalette.ink.opacity(check > 0 ? VignetteClock.mix(0.75, 0.35, check) : 0.75))
                VignetteKit.bar(
                    &row, CGRect(x: rect.minX + 15, y: rect.minY + 8.2, width: index % 2 == 0 ? 44 : 40, height: 2),
                    VignettePalette.dim)
                VignetteKit.check(&row, center: CGPoint(x: rect.maxX - 7, y: rect.midY), size: 2.6, progress: check)
            }
            y += pitch * CGFloat(1 - leave)
        }
    }

    /// The open email: sender, subject, body. A page per lane row, entering
    /// and leaving the way its key sends it.
    private static func drawReader(_ ctx: inout GraphicsContext, t: Double) {
        let ease = VignetteClock.ease
        for index in 0..<4 {
            let open = openness(index, t)
            guard open > 0.001 else { continue }
            // Leaving: up for done, left for skip, down a touch for the walk's
            // end. Entering: from below, or from the right after a skip.
            let closing: Double = {
                switch index {
                case 0: ease(t, events[0].at + 0.26, events[0].at + 0.54)
                case 1: ease(t, events[1].at + 0.26, events[1].at + 0.54)
                case 2: ease(t, events[2].at + 0.16, events[2].at + 0.44)
                default: ease(t, end, end + 0.28)
                }
            }()
            var dx: CGFloat = 0, dy: CGFloat = 0
            if closing > 0 {
                switch index {
                case 0, 1: dy = -14 * CGFloat(closing)
                case 2: dx = -18 * CGFloat(closing)
                default: dy = 4 * CGFloat(closing)
                }
            } else {
                let entering = CGFloat(1 - open)
                if index == 3 { dx = 18 * entering } else { dy = 8 * entering }
            }
            var layer = ctx
            layer.opacity *= open
            layer.translateBy(x: dx, y: dy)
            let shape = Path(roundedRect: page, cornerRadius: 7, style: .continuous)
            layer.fill(shape, with: .color(VignettePalette.card))
            layer.fill(shape, with: .color(VignettePalette.faint))
            layer.stroke(shape, with: .color(VignettePalette.edge), lineWidth: 0.5)

            let tint = index % 2 == 0 ? 0 : 1
            let x = page.minX + 11
            VignetteKit.avatar(&layer, center: CGPoint(x: x + 5, y: page.minY + 13), diameter: 10, tint: tint)
            VignetteKit.bar(
                &layer, CGRect(x: x + 14, y: page.minY + 10, width: [32, 24, 28, 36][index], height: 2.8),
                VignettePalette.ink.opacity(0.85))
            VignetteKit.bar(&layer, CGRect(x: x + 14, y: page.minY + 15.5, width: 20, height: 2.2), VignettePalette.dim)
            VignetteKit.bar(
                &layer, CGRect(x: x, y: page.minY + 28, width: [78, 64, 88, 70][index], height: 4),
                VignettePalette.ink.opacity(0.9))
            let body: [[CGFloat]] = [[112, 104, 114, 60], [108, 116, 90, 72], [114, 98, 110, 48], [104, 112, 84, 66]]
            for (line, width) in body[index].enumerated() {
                VignetteKit.bar(
                    &layer, CGRect(x: x, y: page.minY + 40 + CGFloat(line) * 7, width: width, height: 2.4),
                    VignettePalette.dim.opacity(0.85))
            }
            if index < 2 {
                VignetteKit.check(
                    &layer, center: CGPoint(x: page.maxX - 14, y: page.minY + 13), size: 4.5,
                    progress: ease(t, events[index].at + 0.03, events[index].at + 0.16))
            }
        }
    }
}

// MARK: - unsubscribe

/// UNSUBSCRIBE: `u` on a newsletter, and Passband goes and does it. The
/// sender's mark leaves the email for the tray in the corner, which works
/// (the app's spinner) and lands (a check, and the sender struck through:
/// no more from them). Meanwhile the next email has already stepped up,
/// because there was nothing for you to wait for. Three senders, three
/// presses, so the deck is back where it began.
enum UnsubscribeScene: VignetteDrawing {
    private static let events: [Double] = [0.45, 1.85, 3.25]
    private static let span: Double = 1.4

    /// The first sender struck through in the tray, its check drawn, the
    /// next email already in front: the whole promise in one still.
    static let posterTime: Double = 1.52

    private static let deck = CGPoint(x: 18, y: 36)
    private static let deckWidth: CGFloat = 124
    private static let key = CGPoint(x: 164, y: 42)
    private static let tray = CGRect(x: 140, y: 86, width: 90, height: 22)

    static func draw(_ ctx: inout GraphicsContext, t: Double) {
        let ease = VignetteClock.ease
        let finished = events.filter { t >= $0 + span }.count
        let active = events.first { t >= $0 && t < $0 + span }
        let local = active.map { t - $0 } ?? 0
        let on = active != nil
        // The email goes as its sender's mark leaves it; the deck steps up
        // straight after, while the tray is still working.
        let gone = on ? ease(local, 0.12, 0.4) : 0
        let promote = on ? ease(local, 0.42, 0.8) : 0

        VignetteKit.deckBehind(&ctx, origin: deck, width: deckWidth, first: finished, promote: promote)
        VignetteKit.mail(
            &ctx, origin: deck, width: deckWidth, design: finished % 3, opacity: 1 - gone,
            scale: VignetteClock.mix(1, 0.95, gone), lit: 1 - gone, blank: gone > 0)
        // The card's own mark, drawn apart from the card so it can leave it.
        let markHome = CGPoint(x: deck.x + 13, y: deck.y + 15)
        let markTray = CGPoint(x: tray.minX + 12, y: tray.midY)
        let flight = on ? ease(local, 0.1, 0.42) : 0
        if gone > 0, flight < 1 {
            // A curve that runs right along the email first and then drops
            // into the tray, so the mark is seen to be HANDED over rather
            // than thrown across the card.
            let f = CGFloat(flight), bend = CGPoint(x: markTray.x, y: markHome.y)
            let p = CGPoint(
                x: (1 - f) * (1 - f) * markHome.x + 2 * (1 - f) * f * bend.x + f * f * markTray.x,
                y: (1 - f) * (1 - f) * markHome.y + 2 * (1 - f) * f * bend.y + f * f * markTray.y)
            VignetteKit.avatar(&ctx, center: p, diameter: VignetteClock.mix(10, 9, flight), tint: finished % 3)
        }

        VignetteKit.keycap(
            &ctx, "u", origin: key, width: 18, pressed: events.map { VignetteClock.press(t, at: $0) }.max() ?? 0)

        // The tray card, after UnsubscribeTray: the sender's mark, a line
        // saying who, and on the trailing edge the spinner, then the check.
        guard on else { return }
        let arrive = ease(local, 0.08, 0.32)
        let leave = ease(local, 1.12, 1.36)
        let working = ease(local, 0.42, 0.92)
        let done = ease(local, 0.9, 1.04)

        var layer = ctx
        layer.opacity = arrive * (1 - leave)
        layer.translateBy(x: 10 * CGFloat(1 - arrive) + 8 * CGFloat(leave), y: 0)
        let shape = Path(roundedRect: tray, cornerRadius: 8, style: .continuous)
        layer.fill(shape, with: .color(VignettePalette.cap))
        layer.fill(shape, with: .color(VignettePalette.phosphor.opacity(0.14 * done)))
        layer.stroke(shape, with: .color(VignettePalette.edge), lineWidth: 0.5)
        if done > 0 {
            layer.stroke(shape, with: .color(VignettePalette.phosphor.opacity(0.6 * done)), lineWidth: 0.6)
        }
        if flight >= 1 {
            VignetteKit.avatar(&layer, center: markTray, diameter: 9, tint: finished % 3, opacity: VignetteClock.mix(0.9, 0.45, done))
        }
        // Struck through when it lands: this sender is done sending.
        if done > 0 {
            var slash = Path()
            slash.move(to: CGPoint(x: markTray.x - 4.6, y: markTray.y + 4.6))
            slash.addLine(to: CGPoint(x: markTray.x + 4.6, y: markTray.y - 4.6))
            layer.stroke(
                slash.trimmedPath(from: 0, to: done), with: .color(VignettePalette.phosphorHot),
                style: StrokeStyle(lineWidth: 1.2, lineCap: .round))
        }
        // "Unsubscribing from" in dim, the sender's name in ink.
        VignetteKit.bar(&layer, CGRect(x: tray.minX + 22, y: tray.midY - 1.2, width: 30, height: 2.4), VignettePalette.dim)
        VignetteKit.bar(
            &layer, CGRect(x: tray.minX + 55, y: tray.midY - 1.3, width: 14, height: 2.6), VignettePalette.ink.opacity(0.8))
        let trailing = CGPoint(x: tray.maxX - 10, y: tray.midY)
        // The spinner is the arc's ends travelling round, never a turned
        // picture of one.
        VignetteKit.arc(
            &layer, center: trailing, radius: 4, head: local * 1.6, sweep: 0.3,
            color: VignettePalette.ink.opacity(0.6), lineWidth: 1.1, opacity: working > 0 ? 1 - done : 0)
        VignetteKit.ring(
            &layer, center: trailing, radius: 4.6, progress: done, color: VignettePalette.phosphorHot, lineWidth: 0.9)
        VignetteKit.check(&layer, center: trailing, size: 2.4, progress: ease(local, 0.96, 1.08))
    }
}

// MARK: - clear a stack

/// HOLD E ON A READING STACK: one sender's mail in a pile, named once at the
/// top with its emails listed under. The key goes down and stays down, a ring
/// closes round it at the app's own pace, and when it closes every email in
/// the stack takes its check, top to bottom, and the pile goes. The next
/// sender's stack rises into its place, and half a breath later the key goes
/// down again.
///
/// TWO HOLDS A LOOP, two senders' stacks alternating: one clear takes about
/// two seconds, and a loop that cleared once and then sat still for the rest
/// read as a stall between the stack arriving and the ring. Two stacks also
/// say "stack after stack", which is what the gesture is for.
enum ClearStackScene: VignetteDrawing {
    private static let cycle: Double = VignetteClock.loop / 2
    /// Within a cycle. The app waits 180ms before the ring shows and closes
    /// it in 750ms; those two are the app's own and stay.
    private static let down: Double = 0.42
    private static let ringFrom: Double = down + 0.18
    private static let ringTo: Double = ringFrom + 0.75
    private static let up: Double = ringTo + 0.12
    private static let leaveFrom: Double = ringTo + 0.42
    private static let returnFrom: Double = leaveFrom + 0.16

    /// Held, the ring a little past half.
    static let posterTime: Double = ringFrom + 0.45

    private static let card = CGRect(x: 18, y: 14, width: 132, height: 84)
    /// The two senders, each with its own mail.
    private static let stacks: [(tint: Int, name: CGFloat, rows: [CGFloat])] = [
        (2, 38, [72, 58, 80, 50]),
        (1, 30, [64, 78, 54, 70]),
    ]

    static func draw(_ ctx: inout GraphicsContext, t: Double) {
        let ease = VignetteClock.ease
        let which = t < cycle ? 0 : 1
        let c = t - Double(which) * cycle
        let leaving = ease(c, leaveFrom, leaveFrom + 0.36)
        let back = ease(c, returnFrom, returnFrom + 0.44)
        let returning = c >= returnFrom

        // The stack that leaves lifts and fades; the next sender's rises from
        // a little below into the same place.
        if !returning || leaving < 1 {
            drawPile(&ctx, stacks[which], c: c, dy: -16 * CGFloat(leaving), opacity: 1 - leaving, checks: true)
        }
        if returning {
            drawPile(&ctx, stacks[1 - which], c: c, dy: 12 * CGFloat(1 - back), opacity: back, checks: false)
        }

        // The key, held, and the ring that makes a hold a decision.
        let key = CGPoint(x: 184, y: 47)
        let center = CGPoint(x: key.x + 9, y: key.y + 9)
        let closed = ease(c, ringTo - 0.04, ringTo + 0.04)
        let ringOut = ease(c, up + 0.1, up + 0.36)
        VignetteKit.ring(
            &ctx, center: center, radius: 18, progress: 1, color: VignettePalette.edge, lineWidth: 1.2,
            opacity: ease(c, ringFrom - 0.08, ringFrom) * (1 - ringOut))
        var glow = ctx
        if closed > 0 {
            glow.addFilter(.shadow(color: VignettePalette.phosphor.opacity(0.9 * closed), radius: 4))
        }
        VignetteKit.ring(
            &glow, center: center, radius: 18, progress: VignetteClock.ramp(c, ringFrom, ringTo),
            color: closed > 0.5 ? VignettePalette.phosphorHot : VignettePalette.phosphor, lineWidth: 1.6,
            opacity: 1 - ringOut)
        VignetteKit.keycap(&ctx, "e", origin: key, width: 18, pressed: VignetteClock.hold(c, from: down, to: up))
    }

    /// The pile: two edges peeking under a card that names the sender and
    /// lists its mail.
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

        // The edges under it: there is more than one email here.
        for depth in [2, 1] {
            let inset = CGFloat(depth) * 6
            let edge = CGRect(
                x: card.minX + inset, y: card.minY + CGFloat(depth) * 4, width: card.width - 2 * inset,
                height: card.height)
            let shape = Path(roundedRect: edge, cornerRadius: 7, style: .continuous)
            layer.fill(shape, with: .color(VignettePalette.card.opacity(depth == 1 ? 0.75 : 0.45)))
            layer.stroke(shape, with: .color(VignettePalette.edge), lineWidth: 0.5)
        }
        let shape = Path(roundedRect: card, cornerRadius: 7, style: .continuous)
        layer.fill(shape, with: .color(VignettePalette.card))
        layer.fill(shape, with: .color(VignettePalette.faint))
        layer.stroke(shape, with: .color(VignettePalette.edge), lineWidth: 0.5)
        if lit > 0 {
            layer.stroke(shape, with: .color(VignettePalette.phosphor.opacity(0.85 * lit)), lineWidth: 0.7)
        }

        // The sender, once, at the top.
        VignetteKit.avatar(&layer, center: CGPoint(x: card.minX + 15, y: card.minY + 14), diameter: 11, tint: stack.tint)
        VignetteKit.bar(
            &layer, CGRect(x: card.minX + 26, y: card.minY + 10.5, width: stack.name, height: 3),
            VignettePalette.ink.opacity(0.85))
        VignetteKit.bar(&layer, CGRect(x: card.minX + 26, y: card.minY + 16, width: 22, height: 2.2), VignettePalette.dim)

        // Its mail, one line each, each taking a check in turn as the ring
        // closes: every email, not the one on top.
        for (index, width) in stack.rows.enumerated() {
            let y = card.minY + 32 + CGFloat(index) * 12
            let from = ringTo + 0.02 + 0.07 * Double(index)
            let check = checks ? ease(c, from, from + 0.12) : 0
            layer.fill(
                Path(CGRect(x: card.minX + 10, y: y - 4.5, width: card.width - 20, height: 0.5)),
                with: .color(VignettePalette.edge))
            VignetteKit.bar(
                &layer, CGRect(x: card.minX + 10, y: y, width: width, height: 2.6),
                VignettePalette.ink.opacity(VignetteClock.mix(0.7, 0.32, check)))
            VignetteKit.bar(
                &layer, CGRect(x: card.maxX - 26, y: y + 0.1, width: 12, height: 2.4), VignettePalette.dim.opacity(1 - check))
            VignetteKit.check(&layer, center: CGPoint(x: card.maxX - 14, y: y + 1.3), size: 3, progress: check)
        }
    }
}

// MARK: - smart block

/// SMART BLOCK: a sender you unsubscribed from keeps writing. Their mail comes
/// in from the left, crosses to your inbox, and the notice rises: this sender
/// ignored you. Its middle answer is the one this page is about. Pressed, a
/// rule for that sender lights over the door, and the next two things they
/// send show what "smart" means: the promotion is stopped at the door and
/// struck, and the email you would want (a person's mark, lines of writing)
/// walks through and lands. One sender, two kinds of mail, one of them kept.
///
/// No key: the gesture is a button on a notice, so the actor is that button.
///
/// The seam: two of this sender's emails landed, so the inbox ends two rows
/// further on than it began. Every row is drawn alike but for its sender's
/// dot, and the two new dots settle to the tints of the rows they replaced
/// while the door's light goes out, so the last frame is the first.
enum SmartBlockScene: VignetteDrawing {
    /// The promotion held at the lit door, struck through.
    static let posterTime: Double = 2.74

    private static let sender = 2
    private static let laneMid: CGFloat = 52
    private static let cardSize = CGSize(width: 44, height: 22)
    private static let startX: CGFloat = 40
    private static let doorX: CGFloat = 126
    private static let inbox = CGRect(x: 150, y: 11, width: 80, height: 98)
    private static let pitch: CGFloat = 13
    private static let tray = CGRect(x: 12, y: 86, width: 132, height: 23)

    // The beats.
    private static let firstFrom: Double = 0.2
    private static let firstLand: Double = 0.8
    private static let noticeUp: Double = 0.95
    private static let press: Double = 1.5
    private static let noticeDown: Double = 1.72
    private static let promoFrom: Double = 2.05
    private static let promoStop: Double = 2.5
    private static let promoGone: Double = 2.9
    private static let keptFrom: Double = 3.1
    private static let keptLand: Double = 3.72
    private static let settle: Double = 4.2

    static func draw(_ ctx: inout GraphicsContext, t: Double) {
        let ease = VignetteClock.ease
        let ruleOn = ease(t, press + 0.04, press + 0.3) - ease(t, settle, settle + 0.4)
        let landed1 = ease(t, firstLand, firstLand + 0.24)
        let landed2 = ease(t, keptLand, keptLand + 0.24)
        let seam = ease(t, settle, settle + 0.4)

        drawSender(&ctx)
        drawInbox(&ctx, shift: landed1 + landed2, landed1: landed1, landed2: landed2, seam: seam)
        // Passing mail lights the door for a breath: this one is let through.
        let through = ease(t, keptFrom + 0.26, keptFrom + 0.4) - ease(t, keptFrom + 0.46, keptFrom + 0.66)
        drawDoor(&ctx, on: ruleOn, through: through)

        // 1. The email that should not have come: nothing stops it yet.
        let first = ease(t, firstFrom, firstLand + 0.06)
        if first < 1 || landed1 < 1 {
            drawCard(
                &ctx, x: VignetteClock.mix(startX, inbox.minX + 8, first), promo: true,
                opacity: ease(t, firstFrom, firstFrom + 0.14) * (1 - landed1), scale: VignetteClock.mix(1, 0.9, landed1),
                rise: ease(t, firstLand - 0.22, firstLand + 0.14))
        }

        // 2. Another promotion, after the rule: stopped at the door, struck.
        let reach = ease(t, promoFrom, promoStop)
        let struck = ease(t, promoStop + 0.02, promoStop + 0.18)
        let gone = ease(t, promoGone, promoGone + 0.24)
        if t >= promoFrom, gone < 1 {
            let x = VignetteClock.mix(startX, doorX - 5 - cardSize.width, reach)
            drawCard(
                &ctx, x: x, promo: true,
                opacity: ease(t, promoFrom, promoFrom + 0.14) * VignetteClock.mix(1, 0.5, struck) * (1 - gone),
                scale: VignetteClock.mix(1, 0.94, struck), slash: struck)
        }

        // 3. The email worth having, from the same sender: straight through.
        let kept = ease(t, keptFrom, keptLand + 0.06)
        if t >= keptFrom, landed2 < 1 {
            drawCard(
                &ctx, x: VignetteClock.mix(startX, inbox.minX + 8, kept), promo: false,
                opacity: ease(t, keptFrom, keptFrom + 0.14) * (1 - landed2), scale: VignetteClock.mix(1, 0.9, landed2),
                lit: through, rise: ease(t, keptLand - 0.22, keptLand + 0.14))
        }

        drawNotice(&ctx, t: t)
    }

    /// Who is sending: their mark, and a name under it.
    private static func drawSender(_ ctx: inout GraphicsContext) {
        VignetteKit.avatar(&ctx, center: CGPoint(x: 22, y: laneMid), diameter: 15, tint: sender)
        VignetteKit.bar(&ctx, CGRect(x: 12, y: laneMid + 12, width: 20, height: 2.4), VignettePalette.dim)
    }

    /// The door between a sender and your inbox, and the rule over it: a chip
    /// carrying the sender's mark, dark until Smart block writes it.
    private static func drawDoor(_ ctx: inout GraphicsContext, on: Double, through: Double) {
        var line = Path()
        line.move(to: CGPoint(x: doorX, y: 27))
        line.addLine(to: CGPoint(x: doorX, y: 78))
        ctx.stroke(
            line, with: .color(Color.white.opacity(0.14 * (1 - on))),
            style: StrokeStyle(lineWidth: 0.8, lineCap: .round, dash: [2, 3]))
        if on > 0 {
            var glow = ctx
            glow.addFilter(.shadow(color: VignettePalette.phosphor.opacity(0.85 * on), radius: 3 + 2 * through))
            glow.stroke(
                line,
                with: .color((through > 0.5 ? VignettePalette.phosphorHot : VignettePalette.phosphor).opacity(0.95 * on)),
                style: StrokeStyle(lineWidth: 1.2, lineCap: .round))
        }

        let chip = CGRect(x: doorX - 15, y: 11, width: 30, height: 11)
        let shape = Path(roundedRect: chip, cornerRadius: 5.5)
        var layer = ctx
        if on > 0 { layer.addFilter(.shadow(color: VignettePalette.phosphor.opacity(0.6 * on), radius: 3)) }
        layer.fill(shape, with: .color(VignettePalette.cap))
        layer.fill(shape, with: .color(VignettePalette.phosphor.opacity(0.16 * on)))
        ctx.stroke(shape, with: .color(VignettePalette.edge.opacity(1 - on)), lineWidth: 0.5)
        ctx.stroke(shape, with: .color(VignettePalette.phosphor.opacity(0.9 * on)), lineWidth: 0.7)
        VignetteKit.avatar(
            &ctx, center: CGPoint(x: chip.minX + 7, y: chip.midY), diameter: 5, tint: sender,
            opacity: VignetteClock.mix(0.25, 0.95, on))
        VignetteKit.bar(
            &ctx, CGRect(x: chip.minX + 12, y: chip.midY - 1.1, width: 13, height: 2.2),
            on > 0.5 ? VignettePalette.phosphorHot.opacity(on) : VignettePalette.dim.opacity(0.6))
    }

    /// One email in flight. A promotion is a banner and a line; the email
    /// worth having is a person's mark and writing.
    private static func drawCard(
        _ ctx: inout GraphicsContext, x: CGFloat, promo: Bool, opacity: Double, scale: CGFloat, slash: Double = 0,
        lit: Double = 0, rise: Double = 0
    ) {
        guard opacity > 0.001 else { return }
        let w = cardSize.width * scale, h = cardSize.height * scale
        // Landing, it climbs to the top of the inbox, where new mail goes.
        let mid = VignetteClock.mix(laneMid, inbox.minY + 12, rise)
        let rect = CGRect(x: x + (cardSize.width - w) / 2, y: mid - h / 2, width: w, height: h)
        var layer = ctx
        layer.opacity *= opacity
        let shape = Path(roundedRect: rect, cornerRadius: 5 * scale, style: .continuous)
        layer.fill(shape, with: .color(VignettePalette.card))
        layer.fill(shape, with: .color(VignettePalette.faint))
        layer.stroke(shape, with: .color(VignettePalette.edge), lineWidth: 0.5)
        if lit > 0 {
            layer.stroke(shape, with: .color(VignettePalette.phosphor.opacity(0.85 * lit)), lineWidth: 0.7)
        }
        if promo {
            layer.fill(
                Path(
                    roundedRect: CGRect(x: rect.minX + 5 * scale, y: rect.minY + 4.5 * scale, width: 34 * scale, height: 8 * scale),
                    cornerRadius: 2.5 * scale, style: .continuous),
                with: .color(VignettePalette.avatar[sender].opacity(0.5)))
            VignetteKit.bar(
                &layer, CGRect(x: rect.minX + 5 * scale, y: rect.minY + 15.5 * scale, width: 22 * scale, height: 2.2 * scale),
                VignettePalette.dim)
        } else {
            VignetteKit.avatar(
                &layer, center: CGPoint(x: rect.minX + 9 * scale, y: rect.midY), diameter: 8 * scale, tint: sender)
            VignetteKit.bar(
                &layer, CGRect(x: rect.minX + 17 * scale, y: rect.minY + 7 * scale, width: 18 * scale, height: 2.8 * scale),
                VignettePalette.ink.opacity(0.85))
            VignetteKit.bar(
                &layer, CGRect(x: rect.minX + 17 * scale, y: rect.minY + 12.8 * scale, width: 22 * scale, height: 2.2 * scale),
                VignettePalette.dim)
        }
        // Struck through at the door, in the card's own layer so it fades
        // with it but is not dimmed by it.
        if slash > 0 {
            var mark = Path()
            mark.move(to: CGPoint(x: rect.minX + 8, y: rect.maxY - 4))
            mark.addLine(to: CGPoint(x: rect.maxX - 8, y: rect.minY + 4))
            var hot = ctx
            hot.opacity *= min(1, opacity * 2)
            hot.addFilter(.shadow(color: VignettePalette.phosphor.opacity(0.8), radius: 3))
            hot.stroke(
                mark.trimmedPath(from: 0, to: slash), with: .color(VignettePalette.phosphorHot),
                style: StrokeStyle(lineWidth: 1.5, lineCap: .round))
        }
    }

    /// Your inbox: rows, the newest on top. What lands pushes the rest down.
    private static func drawInbox(
        _ ctx: inout GraphicsContext, shift: Double, landed1: Double, landed2: Double, seam: Double
    ) {
        let shape = Path(roundedRect: inbox, cornerRadius: 7, style: .continuous)
        ctx.fill(shape, with: .color(VignettePalette.card.opacity(0.7)))
        ctx.stroke(shape, with: .color(VignettePalette.edge), lineWidth: 0.5)

        var list = ctx
        list.clip(to: shape)
        VignetteKit.clipFading(&list, inbox, fade: 20)
        // Rows -2 and -1 are the two that land; the rest were there already.
        for index in -2..<8 {
            let appear: Double = index == -1 ? landed1 : index == -2 ? landed2 : 1
            guard appear > 0.001 else { continue }
            let y = inbox.minY + 7 + pitch * (CGFloat(index) + CGFloat(shift))
            guard y < inbox.maxY, y > inbox.minY - pitch else { continue }
            var row = list
            row.opacity *= appear
            let even = index % 2 == 0
            let home = even ? 0 : 1
            let center = CGPoint(x: inbox.minX + 11, y: y + 4.5)
            if index < 0 {
                // This sender's, until the loop comes round.
                VignetteKit.avatar(&row, center: center, diameter: 6, tint: sender, opacity: 0.9 * (1 - seam))
                VignetteKit.avatar(&row, center: center, diameter: 6, tint: home, opacity: 0.9 * seam)
            } else {
                VignetteKit.avatar(&row, center: center, diameter: 6, tint: home)
            }
            VignetteKit.bar(
                &row, CGRect(x: inbox.minX + 19, y: y + 2, width: even ? 30 : 38, height: 2.6),
                VignettePalette.ink.opacity(0.75))
            VignetteKit.bar(&row, CGRect(x: inbox.minX + 19, y: y + 6.8, width: even ? 46 : 36, height: 2), VignettePalette.dim)
        }
    }

    /// The notice, after the app's own: the sender's mark, a line saying they
    /// ignored the unsubscribe, and three answers. The middle one is the
    /// actor, lit at rest and pressed.
    private static func drawNotice(_ ctx: inout GraphicsContext, t: Double) {
        let ease = VignetteClock.ease
        let up = ease(t, noticeUp, noticeUp + 0.28)
        let down = ease(t, noticeDown, noticeDown + 0.26)
        guard up > 0, down < 1 else { return }
        let pressed = VignetteClock.press(t, at: press)

        var layer = ctx
        layer.opacity *= up * (1 - down)
        layer.translateBy(x: 0, y: 8 * CGFloat(1 - up) + 6 * CGFloat(down))
        let shape = Path(roundedRect: tray, cornerRadius: 8, style: .continuous)
        layer.fill(shape, with: .color(VignettePalette.cap))
        layer.stroke(shape, with: .color(VignettePalette.edge), lineWidth: 0.5)
        VignetteKit.avatar(&layer, center: CGPoint(x: tray.minX + 12, y: tray.midY), diameter: 9, tint: sender)
        VignetteKit.bar(&layer, CGRect(x: tray.minX + 20, y: tray.midY - 1.2, width: 20, height: 2.4), VignettePalette.dim)

        // Block sender, Smart block, Keep.
        let pills: [(x: CGFloat, width: CGFloat)] = [(46, 24), (73, 34), (110, 16)]
        for (index, pill) in pills.enumerated() {
            let actor = index == 1
            let sink: CGFloat = actor ? CGFloat(pressed) : 0
            let rect = CGRect(x: tray.minX + pill.x, y: tray.midY - 5.5 + sink, width: pill.width, height: 11)
            let cap = Path(roundedRect: rect, cornerRadius: 5.5)
            var button = layer
            if actor, pressed > 0 {
                button.addFilter(.shadow(color: VignettePalette.phosphor.opacity(0.8 * pressed), radius: 5))
            }
            button.fill(cap, with: .color(VignettePalette.faint))
            if actor {
                button.fill(cap, with: .color(VignettePalette.phosphor.opacity(0.18 + 0.77 * pressed)))
                layer.stroke(cap, with: .color(VignettePalette.phosphor.opacity(0.9)), lineWidth: 0.7)
            } else {
                layer.stroke(cap, with: .color(VignettePalette.edge), lineWidth: 0.5)
            }
            VignetteKit.bar(
                &layer, CGRect(x: rect.minX + 5, y: rect.midY - 1.1, width: rect.width - 10, height: 2.2),
                actor ? (pressed > 0.5 ? Color.white : VignettePalette.phosphorHot) : VignettePalette.dim)
        }
    }
}

// MARK: - parts the 0.0.14 scenes share

extension VignetteKit {
    /// A browser window: three dots, an address field and a rule under them.
    /// Somebody else's page in somebody else's app, which is what two of
    /// these scenes need the eye to know at a glance: this is not mail.
    static func browser(_ ctx: inout GraphicsContext, _ rect: CGRect) {
        let shape = Path(roundedRect: rect, cornerRadius: 7, style: .continuous)
        ctx.fill(shape, with: .color(VignettePalette.card.opacity(0.7)))
        ctx.stroke(shape, with: .color(VignettePalette.edge), lineWidth: 0.5)
        for index in 0..<3 {
            ctx.fill(
                Path(ellipseIn: CGRect(x: rect.minX + 7 + CGFloat(index) * 5.5, y: rect.minY + 5, width: 3, height: 3)),
                with: .color(Color.white.opacity(0.16)))
        }
        let field = CGRect(x: rect.minX + 30, y: rect.minY + 3, width: rect.width - 40, height: 7)
        ctx.fill(Path(roundedRect: field, cornerRadius: 3.5), with: .color(VignettePalette.faint))
        bar(&ctx, CGRect(x: field.minX + 5, y: field.midY - 1, width: 30, height: 2), VignettePalette.dim.opacity(0.7))
        ctx.fill(
            Path(CGRect(x: rect.minX, y: rect.minY + 13, width: rect.width, height: 0.5)),
            with: .color(VignettePalette.edge))
    }

    /// The agent's mark: two four-point glints, after the `sparkles` the
    /// composer sets beside "Your agent drafted this". Nobody's logo.
    static func sparkles(_ ctx: inout GraphicsContext, center: CGPoint, size: CGFloat, color: Color) {
        func glint(_ c: CGPoint, _ r: CGFloat) -> Path {
            var path = Path()
            let k = r * 0.16
            path.move(to: CGPoint(x: c.x, y: c.y - r))
            path.addQuadCurve(to: CGPoint(x: c.x + r, y: c.y), control: CGPoint(x: c.x + k, y: c.y - k))
            path.addQuadCurve(to: CGPoint(x: c.x, y: c.y + r), control: CGPoint(x: c.x + k, y: c.y + k))
            path.addQuadCurve(to: CGPoint(x: c.x - r, y: c.y), control: CGPoint(x: c.x - k, y: c.y + k))
            path.addQuadCurve(to: CGPoint(x: c.x, y: c.y - r), control: CGPoint(x: c.x - k, y: c.y - k))
            return path
        }
        ctx.fill(glint(CGPoint(x: center.x - size * 0.22, y: center.y + size * 0.2), size * 0.8), with: .color(color))
        ctx.fill(glint(CGPoint(x: center.x + size * 0.62, y: center.y - size * 0.6), size * 0.36), with: .color(color))
    }
}

// MARK: - login code

/// LOGIN CODES COME TO YOU: you are in a browser, on a page that wants a code,
/// and Passband is nowhere on screen. The panel drops in at the top of the
/// screen, over the page, carrying the sender's mark and the code. Copy is the
/// one thing it asks for: pressed, it says so with a check, the code goes down
/// into the field that was waiting for it, and the panel leaves. The page
/// takes the code (its button's check) and that is the whole errand.
///
/// The code is six cells in two threes, never numerals. The panel's own cells
/// stay put while their copies travel: copying leaves the original where it
/// was.
///
/// No key: the gesture is a button on a panel, so the actor is that button.
///
/// The seam: the page that took the code lets go of it (the cells empty, the
/// button's check goes out), which leaves the empty form the loop began on.
enum LoginCodeScene: VignetteDrawing {
    /// Copied: the check on the button, the code in the panel and in the
    /// field under it, the panel not yet gone.
    static let posterTime: Double = 2.52

    private static let service = 1
    private static let window = CGRect(x: 26, y: 18, width: 188, height: 92)
    private static let panel = CGRect(x: 80, y: 10, width: 80, height: 51)
    private static let glyph = CGSize(width: 7, height: 10)
    private static let codeMid: CGFloat = 32.5
    private static let fieldTop: CGFloat = 66
    private static let cell = CGSize(width: 12, height: 15)

    // The beats.
    private static let panelIn: Double = 0.45
    private static let press: Double = 1.6
    private static let flyFrom: Double = 1.82
    private static let flyStagger: Double = 0.03
    private static let flySpan: Double = 0.4
    private static let panelOut: Double = 2.64
    private static let accepted: Double = 2.92
    private static let reset: Double = 4.0

    /// Centre of the panel's `index`th code cell, in two threes.
    private static func panelX(_ index: Int) -> CGFloat {
        panel.midX - 30.5 + CGFloat(index) * 10 + (index >= 3 ? 4 : 0) + glyph.width / 2
    }

    /// Centre of the form's `index`th field cell, spaced the same way.
    private static func fieldX(_ index: Int) -> CGFloat {
        window.midX - 45.5 + CGFloat(index) * 15 + (index >= 3 ? 4 : 0) + cell.width / 2
    }

    static func draw(_ ctx: inout GraphicsContext, t: Double) {
        let ease = VignetteClock.ease
        let cleared = ease(t, reset, reset + 0.4)
        let taken = ease(t, accepted, accepted + 0.2) * (1 - cleared)
        let landed = (0..<6).map { ease(t, flyFrom + flyStagger * Double($0), flyFrom + flyStagger * Double($0) + flySpan) }

        drawForm(&ctx, landed: landed, taken: taken, check: ease(t, accepted + 0.04, accepted + 0.22), cleared: cleared)
        drawPanel(&ctx, t: t)

        // The copies on their way down, over everything.
        for (index, p) in landed.enumerated() where p > 0 && p < 1 {
            var hot = ctx
            hot.addFilter(.shadow(color: VignettePalette.phosphor.opacity(0.8), radius: 3))
            drawGlyph(
                &hot,
                center: CGPoint(
                    x: VignetteClock.mix(panelX(index), fieldX(index), p),
                    y: VignetteClock.mix(codeMid, fieldTop + cell.height / 2, p)),
                color: VignettePalette.phosphorHot)
        }
    }

    private static func drawGlyph(_ ctx: inout GraphicsContext, center: CGPoint, color: Color) {
        ctx.fill(
            Path(
                roundedRect: CGRect(
                    x: center.x - glyph.width / 2, y: center.y - glyph.height / 2, width: glyph.width,
                    height: glyph.height),
                cornerRadius: 2.2, style: .continuous),
            with: .color(color))
    }

    /// The other app: a browser on a sign-in page. The site's mark, a line
    /// asking for the code, the six-cell field with the first cell focused,
    /// and the button under it.
    private static func drawForm(
        _ ctx: inout GraphicsContext, landed: [Double], taken: Double, check: Double, cleared: Double
    ) {
        VignetteKit.browser(&ctx, window)
        VignetteKit.avatar(&ctx, center: CGPoint(x: window.midX, y: 40), diameter: 10, tint: service)
        VignetteKit.bar(&ctx, CGRect(x: window.midX - 20, y: 49, width: 40, height: 3), VignettePalette.ink.opacity(0.8))
        VignetteKit.bar(&ctx, CGRect(x: window.midX - 30, y: 55.5, width: 60, height: 2.4), VignettePalette.dim)

        for index in 0..<6 {
            let filled = landed[index] >= 1 ? 1 - cleared : 0
            let rect = CGRect(
                x: fieldX(index) - cell.width / 2, y: fieldTop, width: cell.width, height: cell.height)
            let shape = Path(roundedRect: rect, cornerRadius: 3.5, style: .continuous)
            ctx.fill(shape, with: .color(VignettePalette.faint))
            // The cell the cursor is in, until the code fills it.
            let focus = index == 0 ? 0.42 : 0.16
            ctx.stroke(shape, with: .color(Color.white.opacity(focus * (1 - filled))), lineWidth: 0.6)
            if filled > 0 {
                ctx.stroke(shape, with: .color(VignettePalette.phosphor.opacity(0.7 * filled)), lineWidth: 0.7)
                drawGlyph(
                    &ctx, center: CGPoint(x: rect.midX, y: rect.midY), color: VignettePalette.phosphor.opacity(filled))
            }
        }

        // The page's own button: it takes the code.
        let button = CGRect(x: window.midX - 45.5, y: 88, width: 91, height: 12)
        let cap = Path(roundedRect: button, cornerRadius: 6)
        ctx.fill(cap, with: .color(VignettePalette.faint))
        ctx.fill(cap, with: .color(VignettePalette.phosphor.opacity(0.16 * taken)))
        ctx.stroke(cap, with: .color(VignettePalette.edge.opacity(1 - taken)), lineWidth: 0.5)
        ctx.stroke(cap, with: .color(VignettePalette.phosphor.opacity(0.6 * taken)), lineWidth: 0.6)
        VignetteKit.bar(
            &ctx, CGRect(x: button.midX - 13, y: button.midY - 1.2, width: 26, height: 2.4),
            VignettePalette.dim.opacity(1 - taken))
        VignetteKit.check(
            &ctx, center: CGPoint(x: button.midX, y: button.midY), size: 2.8, progress: check, opacity: taken)
    }

    /// The panel, after LoginCodePanel's card: who sent it, the code, and
    /// Copy beside Dismiss. Copy is the actor, lit at rest and pressed.
    private static func drawPanel(_ ctx: inout GraphicsContext, t: Double) {
        let ease = VignetteClock.ease
        let arrive = ease(t, panelIn, panelIn + 0.32)
        let leave = ease(t, panelOut, panelOut + 0.3)
        guard arrive > 0, leave < 1 else { return }
        let pressed = VignetteClock.press(t, at: press)
        let copied = ease(t, press + 0.1, press + 0.24)

        var layer = ctx
        layer.opacity *= arrive * (1 - leave)
        layer.translateBy(x: 0, y: -9 * CGFloat(1 - arrive) - 7 * CGFloat(leave))
        // It floats: an even shadow all round sets it above the window.
        let shape = Path(roundedRect: panel, cornerRadius: 9, style: .continuous)
        var lifted = layer
        lifted.addFilter(.shadow(color: Color.black.opacity(0.6), radius: 7))
        lifted.fill(shape, with: .color(VignettePalette.cap))
        layer.fill(shape, with: .color(VignettePalette.phosphor.opacity(0.07)))
        layer.stroke(shape, with: .color(Color.white.opacity(0.16)), lineWidth: 0.5)

        VignetteKit.avatar(&layer, center: CGPoint(x: panel.minX + 12, y: panel.minY + 10.5), diameter: 8, tint: service)
        VignetteKit.bar(
            &layer, CGRect(x: panel.minX + 19.5, y: panel.minY + 7, width: 22, height: 2.6),
            VignettePalette.ink.opacity(0.85))
        VignetteKit.bar(&layer, CGRect(x: panel.minX + 19.5, y: panel.minY + 11.8, width: 30, height: 2), VignettePalette.dim)

        for index in 0..<6 {
            drawGlyph(&layer, center: CGPoint(x: panelX(index), y: codeMid), color: VignettePalette.phosphor)
        }

        let copy = CGRect(x: panel.minX + 7, y: panel.minY + 33 + CGFloat(pressed), width: 45, height: 11)
        let cap = Path(roundedRect: copy, cornerRadius: 5.5)
        var button = layer
        if pressed > 0 {
            button.addFilter(.shadow(color: VignettePalette.phosphor.opacity(0.8 * pressed), radius: 5))
        }
        button.fill(cap, with: .color(VignettePalette.phosphor.opacity(0.18 + 0.77 * pressed)))
        layer.stroke(cap, with: .color(VignettePalette.phosphor.opacity(0.9)), lineWidth: 0.7)
        VignetteKit.bar(
            &layer, CGRect(x: copy.midX - 8, y: copy.midY - 1.1, width: 16, height: 2.2),
            (pressed > 0.5 ? Color.white : VignettePalette.phosphorHot).opacity(1 - copied))
        VignetteKit.check(&layer, center: CGPoint(x: copy.midX, y: copy.midY), size: 2.4, progress: copied)

        let dismiss = CGRect(x: panel.minX + 55, y: panel.minY + 33, width: 18, height: 11)
        let other = Path(roundedRect: dismiss, cornerRadius: 5.5)
        layer.fill(other, with: .color(VignettePalette.faint))
        layer.stroke(other, with: .color(VignettePalette.edge), lineWidth: 0.5)
        VignetteKit.bar(
            &layer, CGRect(x: dismiss.minX + 5, y: dismiss.midY - 1.1, width: 8, height: 2.2), VignettePalette.dim)
    }
}

// MARK: - agent write

/// AGENT MCP CAN WRITE: the agent on the left, outside Passband; your
/// composer on the right, empty. The agent wakes, its mark crosses the wire
/// into the composer (the notice that says an agent wrote this), and the
/// draft writes itself on in the actor colour: who it goes to, the subject,
/// the body, line by line. Then the agent is done and the light goes off it:
/// the words cool to ordinary ink, the notice stays, and nothing has been
/// sent. `⌘↵` is yours. Pressed, the draft takes its check and goes, and an
/// empty composer rises where it was.
///
/// The agent's light and the key are never on together, which is the lesson:
/// the agent writes, the human sends.
///
/// The seam: the composer that rises is the empty one the loop began on.
enum AgentWriteScene: VignetteDrawing {
    /// `⌘↵` down, the check drawn on a finished draft that still carries
    /// the agent's notice.
    static let posterTime: Double = 3.24

    private static let agent = CGPoint(x: 30, y: 48)
    private static let card = CGRect(x: 66, y: 10, width: 164, height: 76)
    private static let notice = CGRect(x: 76, y: 43, width: 144, height: 10)
    private static let key = CGPoint(x: 204, y: 91)
    private static let body: [CGFloat] = [138, 124, 142, 72]

    // The beats.
    private static let wake: Double = 0.35
    private static let toFrom: Double = 0.9
    private static let subjectFrom: Double = 1.14
    private static let bodyFrom: Double = 1.5
    private static let bodyStep: Double = 0.24
    private static let rest: Double = 2.55
    private static let press: Double = 3.1
    private static let leaveFrom: Double = 3.36
    private static let nextFrom: Double = 3.46

    static func draw(_ ctx: inout GraphicsContext, t: Double) {
        let ease = VignetteClock.ease
        let working = ease(t, wake, wake + 0.25) - ease(t, rest, rest + 0.35)
        drawAgent(&ctx, on: working)

        let leaving = ease(t, leaveFrom, leaveFrom + 0.3)
        if leaving < 1 {
            drawComposer(&ctx, t: t, written: true, dy: -8 * CGFloat(leaving), opacity: 1 - leaving)
        }
        if t >= nextFrom {
            let back = ease(t, nextFrom, nextFrom + 0.4)
            drawComposer(&ctx, t: t, written: false, dy: 10 * CGFloat(1 - back), opacity: back)
        }

        // The mark crossing over, from the agent into the notice.
        let cross = ease(t, wake + 0.12, wake + 0.5)
        if cross > 0, cross < 1 {
            var hot = ctx
            hot.addFilter(.shadow(color: VignettePalette.phosphor.opacity(0.8), radius: 3))
            VignetteKit.sparkles(
                &hot, center: CGPoint(x: VignetteClock.mix(agent.x, notice.minX + 7, cross), y: agent.y),
                size: VignetteClock.mix(6, 3.2, cross), color: VignettePalette.phosphorHot)
        }

        VignetteKit.keycap(&ctx, "⌘↵", origin: key, width: 26, pressed: VignetteClock.press(t, at: press))
    }

    /// The agent: its mark in a round chip, a name under it, and the wire to
    /// your mailbox. Lit only while it is writing.
    private static func drawAgent(_ ctx: inout GraphicsContext, on: Double) {
        var wire = Path()
        wire.move(to: CGPoint(x: agent.x + 15, y: agent.y))
        wire.addLine(to: CGPoint(x: card.minX - 2, y: agent.y))
        ctx.stroke(
            wire, with: .color(Color.white.opacity(0.16 * (1 - on))),
            style: StrokeStyle(lineWidth: 0.8, lineCap: .round, dash: [2, 3]))
        if on > 0 {
            var glow = ctx
            glow.addFilter(.shadow(color: VignettePalette.phosphor.opacity(0.85 * on), radius: 3))
            glow.stroke(
                wire, with: .color(VignettePalette.phosphor.opacity(0.95 * on)),
                style: StrokeStyle(lineWidth: 1.2, lineCap: .round))
        }

        let chip = Path(ellipseIn: CGRect(x: agent.x - 12, y: agent.y - 12, width: 24, height: 24))
        var layer = ctx
        if on > 0 { layer.addFilter(.shadow(color: VignettePalette.phosphor.opacity(0.6 * on), radius: 4)) }
        layer.fill(chip, with: .color(VignettePalette.cap))
        layer.fill(chip, with: .color(VignettePalette.phosphor.opacity(0.16 * on)))
        ctx.stroke(chip, with: .color(Color.white.opacity(0.14 * (1 - on))), lineWidth: 0.5)
        ctx.stroke(chip, with: .color(VignettePalette.phosphor.opacity(0.9 * on)), lineWidth: 0.7)
        VignetteKit.sparkles(
            &ctx, center: agent, size: 6, color: VignettePalette.phosphor.opacity(VignetteClock.mix(0.6, 1, on)))
        VignetteKit.bar(&ctx, CGRect(x: agent.x - 10, y: agent.y + 17, width: 20, height: 2.4), VignettePalette.dim)
    }

    /// One written thing: phosphor while the agent is at it, ink once the
    /// agent has stopped, with a hot tip where the writing is.
    private static func write(_ ctx: inout GraphicsContext, _ rect: CGRect, progress: Double, cool: Double, ink: Double) {
        guard progress > 0 else { return }
        let drawn = CGRect(x: rect.minX, y: rect.minY, width: max(rect.height, rect.width * CGFloat(progress)), height: rect.height)
        VignetteKit.bar(&ctx, drawn, VignettePalette.phosphor.opacity(1 - cool))
        VignetteKit.bar(&ctx, drawn, VignettePalette.ink.opacity(ink * cool))
        if progress < 1 {
            var hot = ctx
            hot.addFilter(.shadow(color: VignettePalette.phosphor.opacity(0.9), radius: 2.5))
            VignetteKit.bar(
                &hot, CGRect(x: drawn.maxX - rect.height, y: rect.minY, width: rect.height, height: rect.height),
                VignettePalette.phosphorHot)
        }
    }

    /// The composer: To, subject, body. `written` is the one the agent
    /// fills; the other is the empty one that takes its place.
    private static func drawComposer(_ ctx: inout GraphicsContext, t: Double, written: Bool, dy: CGFloat, opacity: Double) {
        guard opacity > 0.001 else { return }
        let ease = VignetteClock.ease
        var layer = ctx
        layer.opacity *= opacity
        layer.translateBy(x: 0, y: dy)
        let shape = Path(roundedRect: card, cornerRadius: 7, style: .continuous)
        layer.fill(shape, with: .color(VignettePalette.card))
        layer.fill(shape, with: .color(VignettePalette.faint))
        layer.stroke(shape, with: .color(VignettePalette.edge), lineWidth: 0.5)

        // The fields' own labels and the rules between them: an empty
        // composer is these and nothing else.
        let x = card.minX + 10
        VignetteKit.bar(&layer, CGRect(x: x, y: 18.9, width: 7, height: 2.2), VignettePalette.dim)
        VignetteKit.bar(&layer, CGRect(x: x, y: 31.9, width: 12, height: 2.2), VignettePalette.dim)
        for y in [27, 39] as [CGFloat] {
            layer.fill(Path(CGRect(x: x, y: y, width: card.width - 20, height: 0.5)), with: .color(VignettePalette.edge))
        }
        guard written else { return }
        let cool = ease(t, rest, rest + 0.35)

        // The notice: the mark that travels with the words, and stays.
        let marked = ease(t, wake + 0.42, wake + 0.62)
        if marked > 0 {
            var strip = layer
            strip.opacity *= marked
            let pill = Path(roundedRect: notice, cornerRadius: 4, style: .continuous)
            strip.fill(pill, with: .color(VignettePalette.phosphor.opacity(0.14)))
            strip.stroke(pill, with: .color(VignettePalette.phosphor.opacity(0.45)), lineWidth: 0.5)
            VignetteKit.sparkles(
                &strip, center: CGPoint(x: notice.minX + 7, y: notice.midY), size: 3.2, color: VignettePalette.phosphorHot)
            VignetteKit.bar(
                &strip, CGRect(x: notice.minX + 14, y: notice.midY - 1.1, width: 84, height: 2.2),
                VignettePalette.phosphorHot.opacity(0.75))
        }

        // Who it goes to: a person's chip.
        let to = ease(t, toFrom, toFrom + 0.2)
        if to > 0 {
            var chip = layer
            chip.opacity *= to
            let rect = CGRect(x: x + 12, y: 15.5, width: 36, height: 9)
            let pill = Path(roundedRect: rect, cornerRadius: 4.5)
            chip.fill(pill, with: .color(VignettePalette.faint))
            chip.stroke(pill, with: .color(VignettePalette.edge.opacity(cool)), lineWidth: 0.5)
            chip.stroke(pill, with: .color(VignettePalette.phosphor.opacity(0.8 * (1 - cool))), lineWidth: 0.6)
            VignetteKit.avatar(&chip, center: CGPoint(x: rect.minX + 5.5, y: rect.midY), diameter: 5, tint: 2)
            write(&chip, CGRect(x: rect.minX + 10.5, y: rect.midY - 1.1, width: 20, height: 2.2), progress: to, cool: cool, ink: 0.8)
        }
        write(
            &layer, CGRect(x: x + 17, y: 31.4, width: 76, height: 3.2),
            progress: VignetteClock.ramp(t, subjectFrom, subjectFrom + 0.28), cool: cool, ink: 0.88)
        for (index, width) in body.enumerated() {
            let from = bodyFrom + bodyStep * Double(index)
            write(
                &layer, CGRect(x: x, y: 58 + CGFloat(index) * 6.5, width: width, height: 2.6),
                progress: VignetteClock.ramp(t, from, from + 0.22), cool: cool, ink: 0.6)
        }

        // Sent: the check, as the page takes one in flow mode.
        VignetteKit.check(
            &layer, center: CGPoint(x: card.maxX - 14, y: 20), size: 4, progress: ease(t, press + 0.03, press + 0.18))
    }
}

// MARK: - smart unsubscribe

/// SMART UNSUBSCRIBE: `u` on a newsletter whose sender does not make it one
/// click. Their page opens beside the email: a preference page, every list
/// ticked, a button at the bottom. This used to be handed back to you. Now a
/// pointer in the actor colour, nobody's hand on it, works down the page:
/// one list unticked, the next, the last, then the page's own button. The
/// page answers with its confirmation, the sender's mark on the email is
/// struck through, and both go. The next newsletter rises where the last one
/// was.
///
/// The key is pressed once, in the first half second, and never again: every
/// thing after it is done for you. The 0.0.12 scene is a deck and a tray;
/// this one is the web page, which is the part that is new.
///
/// The seam: every newsletter is drawn alike, so the one that rises is the
/// one the loop began on, and the page has gone the way it came.
enum SmartUnsubscribeScene: VignetteDrawing {
    /// The pointer down on the second list, the first already unticked, the
    /// third still waiting: a page being worked, and by what.
    static let posterTime: Double = 1.86

    private static let sender = 1
    private static let mail = CGRect(x: 10, y: 30, width: 78, height: 30)
    private static let key = CGPoint(x: 40, y: 72)
    private static let page = CGRect(x: 102, y: 10, width: 128, height: 100)
    private static let rows: [(mid: CGFloat, width: CGFloat)] = [(50, 60), (62, 46), (74, 68)]
    private static let button = CGRect(x: 112, y: 86, width: 50, height: 12)

    // The beats.
    private static let press: Double = 0.3
    private static let pageIn: Double = 0.42
    private static let clicks: [Double] = [1.32, 1.82, 2.32]
    private static let submit: Double = 2.95
    private static let done: Double = 3.12
    private static let leave: Double = 4.05

    /// Where the pointer's tip rests on a tick box, and on the button.
    private static func target(_ index: Int) -> CGPoint {
        index < rows.count
            ? CGPoint(x: page.minX + 15.5, y: rows[index].mid + 1.5)
            : CGPoint(x: button.midX + 8, y: button.midY + 1.5)
    }

    static func draw(_ ctx: inout GraphicsContext, t: Double) {
        let ease = VignetteClock.ease
        let finished = ease(t, done, done + 0.25)
        let going = ease(t, leave, leave + 0.3)

        // The newsletter, struck when the page says it is done; then the
        // next one, rising as it lifts away.
        drawMail(
            &ctx, dy: -12 * CGFloat(going), opacity: 1 - going, struck: ease(t, done + 0.14, done + 0.34))
        if t >= leave + 0.08 {
            let back = ease(t, leave + 0.08, leave + 0.46)
            drawMail(&ctx, dy: 10 * CGFloat(1 - back), opacity: back, struck: 0)
        }
        VignetteKit.keycap(&ctx, "u", origin: key, width: 18, pressed: VignetteClock.press(t, at: press))

        let arrive = ease(t, pageIn, pageIn + 0.3)
        guard arrive > 0, going < 1 else { return }
        var layer = ctx
        layer.opacity *= arrive * (1 - going)
        layer.translateBy(x: -10 * CGFloat(1 - arrive) + 8 * CGFloat(going), y: 0)
        drawPage(&layer, t: t, finished: finished)

        // The pointer: in with the page, from box to box to button, and out
        // once there is nothing left to press.
        var at = CGPoint(x: page.maxX - 34, y: page.maxY - 30)
        let stops = clicks + [submit]
        for (index, click) in stops.enumerated() {
            let move = ease(t, click - (index == stops.count - 1 ? 0.42 : 0.34), click - 0.06)
            let to = target(index)
            at = CGPoint(x: VignetteClock.mix(at.x, to.x, move), y: VignetteClock.mix(at.y, to.y, move))
        }
        let down = stops.map { VignetteClock.press(t, at: $0) }.max() ?? 0
        drawPointer(&layer, tip: at, down: down, opacity: ease(t, pageIn + 0.2, pageIn + 0.44) * (1 - finished))
    }

    /// One newsletter: the sender's mark, a title, a line. Lit, because it
    /// is the one the key acts on, until its sender is struck.
    private static func drawMail(_ ctx: inout GraphicsContext, dy: CGFloat, opacity: Double, struck: Double) {
        guard opacity > 0.001 else { return }
        var layer = ctx
        layer.opacity *= opacity
        layer.translateBy(x: 0, y: dy)
        let shape = Path(roundedRect: mail, cornerRadius: 6, style: .continuous)
        layer.fill(shape, with: .color(VignettePalette.card))
        layer.fill(shape, with: .color(VignettePalette.faint))
        layer.stroke(shape, with: .color(VignettePalette.edge), lineWidth: 0.5)
        layer.stroke(shape, with: .color(VignettePalette.phosphor.opacity(0.85 * (1 - struck))), lineWidth: 0.7)
        let mark = CGPoint(x: mail.minX + 13, y: mail.midY)
        VignetteKit.avatar(&layer, center: mark, diameter: 10, tint: sender, opacity: VignetteClock.mix(0.9, 0.4, struck))
        VignetteKit.bar(
            &layer, CGRect(x: mail.minX + 24, y: mail.minY + 10, width: 30, height: 3),
            VignettePalette.ink.opacity(VignetteClock.mix(0.8, 0.4, struck)))
        VignetteKit.bar(
            &layer, CGRect(x: mail.minX + 24, y: mail.minY + 17, width: 44, height: 2.5),
            VignettePalette.dim.opacity(VignetteClock.mix(1, 0.6, struck)))
        if struck > 0 {
            var slash = Path()
            slash.move(to: CGPoint(x: mark.x - 5, y: mark.y + 5))
            slash.addLine(to: CGPoint(x: mark.x + 5, y: mark.y - 5))
            layer.stroke(
                slash.trimmedPath(from: 0, to: struck), with: .color(VignettePalette.phosphorHot),
                style: StrokeStyle(lineWidth: 1.3, lineCap: .round))
        }
    }

    /// The sender's preference page: a heading, a line, three lists each
    /// with its tick box, and the button that saves it. Then the page it
    /// turns into: a ring and a check.
    private static func drawPage(_ ctx: inout GraphicsContext, t: Double, finished: Double) {
        let ease = VignetteClock.ease
        VignetteKit.browser(&ctx, page)
        var form = ctx
        form.opacity *= 1 - finished
        let x = page.minX + 10
        VignetteKit.bar(&form, CGRect(x: x, y: 30, width: 46, height: 3.2), VignettePalette.ink.opacity(0.85))
        VignetteKit.bar(&form, CGRect(x: x, y: 36.5, width: 78, height: 2.4), VignettePalette.dim)

        for (index, row) in rows.enumerated() {
            let ticked = 1 - ease(t, clicks[index] + 0.05, clicks[index] + 0.17)
            let box = CGRect(x: x, y: row.mid - 4, width: 8, height: 8)
            let shape = Path(roundedRect: box, cornerRadius: 2.2, style: .continuous)
            form.fill(shape, with: .color(VignettePalette.ink.opacity(0.82 * ticked)))
            form.stroke(shape, with: .color(Color.white.opacity(0.3 * (1 - ticked))), lineWidth: 0.6)
            if ticked > 0 {
                var tick = Path()
                tick.move(to: CGPoint(x: box.midX - 2.3, y: box.midY + 0.1))
                tick.addLine(to: CGPoint(x: box.midX - 0.7, y: box.midY + 1.7))
                tick.addLine(to: CGPoint(x: box.midX + 2.4, y: box.midY - 1.8))
                form.stroke(
                    tick.trimmedPath(from: 0, to: ticked), with: .color(VignettePalette.groundBottom.opacity(ticked)),
                    style: StrokeStyle(lineWidth: 1.2, lineCap: .round, lineJoin: .round))
            }
            VignetteKit.bar(
                &form, CGRect(x: x + 14, y: row.mid - 1.4, width: row.width, height: 2.8),
                VignettePalette.ink.opacity(VignetteClock.mix(0.28, 0.75, ticked)))
        }

        // The page's button, pressed by the pointer and not by you.
        let pressed = VignetteClock.press(t, at: submit)
        let rect = button.offsetBy(dx: 0, dy: CGFloat(pressed))
        let cap = Path(roundedRect: rect, cornerRadius: 6)
        var glow = form
        if pressed > 0 { glow.addFilter(.shadow(color: VignettePalette.phosphor.opacity(0.8 * pressed), radius: 5)) }
        glow.fill(cap, with: .color(Color.white.opacity(0.1)))
        glow.fill(cap, with: .color(VignettePalette.phosphor.opacity(0.95 * pressed)))
        form.stroke(cap, with: .color(Color.white.opacity(0.2 * (1 - pressed))), lineWidth: 0.5)
        VignetteKit.bar(
            &form, CGRect(x: rect.minX + 9, y: rect.midY - 1.2, width: rect.width - 18, height: 2.4),
            pressed > 0.5 ? Color.white : VignettePalette.ink.opacity(0.75))

        // Done: what the sender's page says back.
        let center = CGPoint(x: page.midX, y: page.minY + 13 + (page.height - 13) / 2 - 4)
        var hot = ctx
        hot.addFilter(.shadow(color: VignettePalette.phosphor.opacity(0.7 * finished), radius: 4))
        VignetteKit.ring(
            &hot, center: center, radius: 11, progress: ease(t, done + 0.14, done + 0.4),
            color: VignettePalette.phosphorHot, lineWidth: 1.4)
        VignetteKit.check(&ctx, center: center, size: 5, progress: ease(t, done + 0.32, done + 0.48))
        VignetteKit.bar(
            &ctx, CGRect(x: center.x - 19, y: center.y + 18, width: 38, height: 2.6),
            VignettePalette.ink.opacity(0.7 * ease(t, done + 0.3, done + 0.5)))
    }

    /// The pointer: an arrow in the actor colour with an even light edge.
    /// Down, it shrinks a touch toward its tip and glows.
    private static func drawPointer(_ ctx: inout GraphicsContext, tip: CGPoint, down: Double, opacity: Double) {
        guard opacity > 0.001 else { return }
        let s = 0.86 * VignetteClock.mix(1, 0.93, down)
        let shape: [(CGFloat, CGFloat)] = [(0, 0), (0, 10.4), (2.7, 8), (4.6, 12), (6.5, 11.1), (4.6, 7.2), (8, 7.2)]
        var arrow = Path()
        for (index, p) in shape.enumerated() {
            let point = CGPoint(x: tip.x + p.0 * s, y: tip.y + p.1 * s)
            if index == 0 { arrow.move(to: point) } else { arrow.addLine(to: point) }
        }
        arrow.closeSubpath()
        var layer = ctx
        layer.opacity *= opacity
        layer.addFilter(.shadow(color: VignettePalette.phosphor.opacity(0.45 + 0.5 * down), radius: 2.5 + 2.5 * down))
        layer.fill(arrow, with: .color(down > 0.5 ? VignettePalette.phosphorHot : VignettePalette.phosphor))
        layer.stroke(
            arrow, with: .color(Color.white.opacity(0.9)), style: StrokeStyle(lineWidth: 0.6, lineJoin: .round))
    }
}
