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
        case .smartClear: SmartClearScene.self
        case .autoTranslate: AutoTranslateScene.self
        case .archiveSearch: ArchiveSearchScene.self
        case .autoDone: AutoDoneScene.self
        case .calendar: CalendarScene.self
        case .eventReminder: EventReminderScene.self
        case .outlookSignIn: OutlookSignInScene.self
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

    private static func drawPointer(_ ctx: inout GraphicsContext, tip: CGPoint, down: Double, opacity: Double) {
        VignetteKit.pointer(&ctx, tip: tip, down: down, opacity: opacity)
    }
}

// MARK: - parts the 0.0.15 scenes share

extension VignetteKit {
    /// A small calendar page: an event, as a mark rather than a date.
    static func calendarMark(_ ctx: inout GraphicsContext, center: CGPoint, size: CGFloat, color: Color) {
        let rect = CGRect(x: center.x - size / 2, y: center.y - size * 0.42, width: size, height: size * 0.9)
        let page = Path(roundedRect: rect, cornerRadius: size * 0.18, style: .continuous)
        ctx.stroke(page, with: .color(color), lineWidth: 0.6)
        ctx.fill(
            Path(CGRect(x: rect.minX + 0.3, y: rect.minY + size * 0.24, width: rect.width - 0.6, height: 0.6)),
            with: .color(color))
        for dx in [-size * 0.22, size * 0.22] {
            ctx.fill(
                Path(roundedRect: CGRect(x: center.x + dx - 0.35, y: rect.minY - size * 0.12, width: 0.7, height: size * 0.26),
                     cornerRadius: 0.35),
                with: .color(color))
        }
    }

    /// A price tag: an offer, the kind of mail that ends.
    static func tagMark(_ ctx: inout GraphicsContext, center: CGPoint, size: CGFloat, color: Color) {
        let h = size * 0.38, w = size / 2
        var tag = Path()
        tag.move(to: CGPoint(x: center.x - w, y: center.y))
        tag.addLine(to: CGPoint(x: center.x - w * 0.45, y: center.y - h))
        tag.addLine(to: CGPoint(x: center.x + w, y: center.y - h))
        tag.addLine(to: CGPoint(x: center.x + w, y: center.y + h))
        tag.addLine(to: CGPoint(x: center.x - w * 0.45, y: center.y + h))
        tag.closeSubpath()
        ctx.stroke(tag, with: .color(color), style: StrokeStyle(lineWidth: 0.6, lineJoin: .round))
        ctx.fill(
            Path(ellipseIn: CGRect(x: center.x - w * 0.42, y: center.y - 0.55, width: 1.1, height: 1.1)),
            with: .color(color))
    }

    /// A parcel: an order, the kind of mail that is over once it ships.
    static func parcelMark(_ ctx: inout GraphicsContext, center: CGPoint, size: CGFloat, color: Color) {
        let rect = CGRect(x: center.x - size * 0.45, y: center.y - size * 0.4, width: size * 0.9, height: size * 0.8)
        ctx.stroke(Path(roundedRect: rect, cornerRadius: size * 0.12, style: .continuous), with: .color(color), lineWidth: 0.6)
        ctx.fill(Path(CGRect(x: rect.minX, y: rect.minY + rect.height * 0.32, width: rect.width, height: 0.6)), with: .color(color))
        ctx.fill(Path(CGRect(x: center.x - 0.3, y: rect.minY, width: 0.6, height: rect.height * 0.32)), with: .color(color))
    }

    /// A rounded button with a bar for its name, lit and pressable: the
    /// actor when a scene's gesture is a click rather than a key.
    static func pill(
        _ ctx: inout GraphicsContext, _ rect: CGRect, label: CGFloat, labelX: CGFloat? = nil, pressed: Double,
        actor: Bool = true
    ) {
        let rect = rect.offsetBy(dx: 0, dy: CGFloat(pressed))
        let cap = Path(roundedRect: rect, cornerRadius: rect.height / 2)
        var glow = ctx
        if pressed > 0 { glow.addFilter(.shadow(color: VignettePalette.phosphor.opacity(0.8 * pressed), radius: 5)) }
        glow.fill(cap, with: .color(VignettePalette.faint))
        if actor {
            glow.fill(cap, with: .color(VignettePalette.phosphor.opacity(0.18 + 0.77 * pressed)))
            ctx.stroke(cap, with: .color(VignettePalette.phosphor.opacity(0.9)), lineWidth: 0.7)
        } else {
            ctx.stroke(cap, with: .color(VignettePalette.edge), lineWidth: 0.5)
        }
        bar(
            &ctx, CGRect(x: labelX ?? (rect.minX + 5), y: rect.midY - 1.1, width: label, height: 2.2),
            actor ? (pressed > 0.5 ? Color.white : VignettePalette.phosphorHot) : VignettePalette.dim)
    }
}

// MARK: - smart clear

/// SMART CLEAR: the Reading zone, six senders' cards, and three of them have
/// run their course. Each says why with a small mark where its count would
/// be: an event that has happened (a calendar), an offer that ended (a tag),
/// an order that shipped (a parcel). One press on the zone's own pill, it
/// works for a breath, and those three take their checks and go while the
/// rest stay exactly where they were. Then the receipt rises in the corner,
/// as the app's does: a headline, a line per reason carrying the same mark
/// and its count, and an Undo that drains to say how long it lasts.
///
/// The marks are the lesson: the cards that went are the ones that said they
/// were over, and the receipt says so in the same words.
///
/// The seam: the receipt goes, and new mail rises into the three gaps, drawn
/// as the cards the loop began on.
enum SmartClearScene: VignetteDrawing {
    /// The receipt up with its three lines, the three gaps it accounts for.
    static let posterTime: Double = 2.95

    private enum Reason { case event, offer, parcel }
    private struct Card {
        let tint: Int
        let name: CGFloat
        let lines: (CGFloat, CGFloat)
        let reason: Reason?
    }

    /// Three rows of three, read across: the expired ones are wherever
    /// they happen to be, not lined up for the camera.
    private static let cards: [Card] = [
        Card(tint: 0, name: 26, lines: (30, 22), reason: nil),
        Card(tint: 2, name: 20, lines: (32, 18), reason: .offer),
        Card(tint: 1, name: 28, lines: (24, 30), reason: nil),
        Card(tint: 1, name: 30, lines: (26, 30), reason: .event),
        Card(tint: 2, name: 22, lines: (30, 24), reason: nil),
        Card(tint: 0, name: 24, lines: (28, 20), reason: nil),
        Card(tint: 2, name: 18, lines: (32, 26), reason: nil),
        Card(tint: 1, name: 26, lines: (22, 30), reason: nil),
        Card(tint: 0, name: 20, lines: (30, 22), reason: .parcel),
    ]
    private static let cardSize = CGSize(width: 70, height: 24)
    private static let pill = CGRect(x: 180, y: 10, width: 50, height: 11)
    /// The receipt is a corner card OVER the board, as the app's is, set
    /// square on the last column so nothing peeks round it.
    private static let receipt = CGRect(x: 160, y: 26, width: 70, height: 80)

    // The beats.
    private static let press: Double = 0.55
    private static let checks: [Double] = [1.2, 1.32, 1.44]
    private static let goneFrom: Double = 1.62
    private static let receiptIn: Double = 1.95
    private static let receiptOut: Double = 3.85
    private static let refill: Double = 4.12

    private static func origin(_ index: Int) -> CGPoint {
        CGPoint(x: 10 + CGFloat(index % 3) * (cardSize.width + 5), y: 26 + CGFloat(index / 3) * (cardSize.height + 4))
    }

    static func draw(_ ctx: inout GraphicsContext, t: Double) {
        let ease = VignetteClock.ease
        // The zone's heading, and its pill in the corner.
        VignetteKit.bar(&ctx, CGRect(x: 11, y: 14.2, width: 28, height: 3), VignettePalette.ink.opacity(0.55))
        VignetteKit.bar(&ctx, CGRect(x: 43, y: 14.5, width: 14, height: 2.4), VignettePalette.dim)
        drawPill(&ctx, t: t)

        var expiredSeen = 0
        for (index, card) in cards.enumerated() {
            if card.reason == nil {
                drawCard(&ctx, card, at: origin(index), opacity: 1, dy: 0, check: 0)
                continue
            }
            let at = checks[expiredSeen]
            expiredSeen += 1
            let check = ease(t, at, at + 0.14)
            let gone = ease(t, goneFrom, goneFrom + 0.3)
            let back = ease(t, refill + 0.05 * Double(expiredSeen), refill + 0.05 * Double(expiredSeen) + 0.32)
            if gone < 1 {
                drawCard(&ctx, card, at: origin(index), opacity: 1 - gone, dy: -6 * CGFloat(gone), check: check)
            } else if back > 0 {
                drawCard(&ctx, card, at: origin(index), opacity: back, dy: 8 * CGFloat(1 - back), check: 0)
            }
        }
        drawReceipt(&ctx, t: t)
    }

    /// The zone's pill: a check-stack mark and its name, then the app's
    /// spinner while it tidies up.
    private static func drawPill(_ ctx: inout GraphicsContext, t: Double) {
        let ease = VignetteClock.ease
        let pressed = VignetteClock.press(t, at: press)
        VignetteKit.pill(&ctx, pill, label: 28, labelX: pill.minX + 15, pressed: pressed)
        let working = ease(t, press + 0.12, press + 0.22) - ease(t, checks[0] - 0.1, checks[0])
        let mark = CGPoint(x: pill.minX + 8.5, y: pill.midY + CGFloat(pressed))
        VignetteKit.check(&ctx, center: mark, size: 2.4, progress: 1, opacity: 1 - working)
        VignetteKit.arc(
            &ctx, center: mark, radius: 2.6, head: t * 1.6, sweep: 0.3, color: VignettePalette.phosphorHot,
            lineWidth: 0.9, opacity: working)
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
    /// name and its count across the top, two lines of summary under.
    private static func drawCard(
        _ ctx: inout GraphicsContext, _ card: Card, at origin: CGPoint, opacity: Double, dy: CGFloat, check: Double
    ) {
        guard opacity > 0.001 else { return }
        var layer = ctx
        layer.opacity *= opacity
        layer.translateBy(x: 0, y: dy)
        let rect = CGRect(origin: origin, size: cardSize)
        let shape = Path(roundedRect: rect, cornerRadius: 6, style: .continuous)
        layer.fill(shape, with: .color(VignettePalette.card))
        layer.fill(shape, with: .color(VignettePalette.faint))
        layer.stroke(shape, with: .color(VignettePalette.edge), lineWidth: 0.5)
        if check > 0 {
            layer.stroke(shape, with: .color(VignettePalette.phosphor.opacity(0.85 * check)), lineWidth: 0.7)
        }
        let hero = CGRect(x: rect.minX + 4, y: rect.minY + 4, width: 16, height: 16)
        layer.fill(
            Path(roundedRect: hero, cornerRadius: 3.5, style: .continuous),
            with: .color(VignettePalette.avatar[card.tint].opacity(VignetteClock.mix(0.5, 0.28, check))))
        let x = rect.minX + 24
        VignetteKit.bar(
            &layer, CGRect(x: x, y: rect.minY + 5.5, width: card.name, height: 2.8),
            VignettePalette.ink.opacity(VignetteClock.mix(0.85, 0.4, check)))
        VignetteKit.bar(&layer, CGRect(x: x, y: rect.minY + 12, width: card.lines.0, height: 2.2), VignettePalette.dim)
        VignetteKit.bar(&layer, CGRect(x: x, y: rect.minY + 16.5, width: card.lines.1, height: 2.2), VignettePalette.dim)
        let corner = CGPoint(x: rect.maxX - 8, y: rect.minY + 7)
        if let reason = card.reason {
            reasonMark(&layer, reason, center: corner, size: 6, color: VignettePalette.ink.opacity(0.6 * (1 - check)))
            VignetteKit.check(&layer, center: corner, size: 3, progress: check)
        } else {
            VignetteKit.bar(&layer, CGRect(x: rect.maxX - 13, y: rect.minY + 6, width: 8, height: 2.2), VignettePalette.dim)
        }
    }

    /// The receipt, after SmartClearReceiptCard: its name, how many went,
    /// from how many senders, a line per reason (the mark, a count, the
    /// words), and Undo, draining.
    private static func drawReceipt(_ ctx: inout GraphicsContext, t: Double) {
        let ease = VignetteClock.ease
        let arrive = ease(t, receiptIn, receiptIn + 0.32)
        let leave = ease(t, receiptOut, receiptOut + 0.3)
        guard arrive > 0, leave < 1 else { return }
        var layer = ctx
        layer.opacity *= arrive * (1 - leave)
        layer.translateBy(x: 10 * CGFloat(1 - arrive) + 8 * CGFloat(leave), y: 0)
        let shape = Path(roundedRect: receipt, cornerRadius: 8, style: .continuous)
        var lifted = layer
        lifted.addFilter(.shadow(color: Color.black.opacity(0.5), radius: 6))
        lifted.fill(shape, with: .color(VignettePalette.cap))
        layer.stroke(shape, with: .color(Color.white.opacity(0.14)), lineWidth: 0.5)

        let x = receipt.minX + 8
        VignetteKit.check(&layer, center: CGPoint(x: x + 2.4, y: receipt.minY + 9.5), size: 2.2, progress: 1, opacity: 0.8)
        VignetteKit.bar(&layer, CGRect(x: x + 7, y: receipt.minY + 8.4, width: 24, height: 2.2), VignettePalette.dim)
        VignetteKit.bar(&layer, CGRect(x: x, y: receipt.minY + 17, width: 46, height: 4.6), VignettePalette.ink.opacity(0.9))
        VignetteKit.bar(&layer, CGRect(x: x, y: receipt.minY + 25.5, width: 26, height: 2.2), VignettePalette.dim)

        let reasons: [(Reason, CGFloat)] = [(.event, 40), (.offer, 30), (.parcel, 36)]
        for (index, line) in reasons.enumerated() {
            let shown = ease(t, receiptIn + 0.24 + 0.1 * Double(index), receiptIn + 0.44 + 0.1 * Double(index))
            var row = layer
            row.opacity *= shown
            let y = receipt.minY + 38 + CGFloat(index) * 9
            reasonMark(&row, line.0, center: CGPoint(x: x + 3, y: y), size: 5.4, color: VignettePalette.phosphorHot)
            VignetteKit.bar(&row, CGRect(x: x + 10, y: y - 1.3, width: 4, height: 2.6), VignettePalette.ink.opacity(0.85))
            VignetteKit.bar(&row, CGRect(x: x + 17, y: y - 1.1, width: line.1, height: 2.2), VignettePalette.dim)
        }

        // Undo, and how long it has left: the rule under it drains.
        VignetteKit.bar(&layer, CGRect(x: x, y: receipt.maxY - 12, width: 14, height: 2.4), VignettePalette.phosphorHot.opacity(0.85))
        let left = 1 - VignetteClock.ramp(t, receiptIn + 0.3, receiptOut + 0.3)
        let track = CGRect(x: x, y: receipt.maxY - 6, width: receipt.width - 16, height: 1)
        VignetteKit.bar(&layer, track, VignettePalette.faint)
        VignetteKit.bar(
            &layer, CGRect(x: track.minX, y: track.minY, width: max(1, track.width * CGFloat(left)), height: 1),
            VignettePalette.phosphor.opacity(0.7))
    }
}

// MARK: - auto-translate

/// AUTO-TRANSLATE: an email in Japanese, open in the reader. Its words are
/// square glyphs, set solid the way the script sets, so it reads as another
/// language at a glance without a character on screen. Under the header the
/// caption says it is translating (the app's own strip: a mark, a line, a
/// spinner). Then the English lands where the Japanese was, line by line
/// from the subject down, as words with spaces between them, in the actor
/// colour, cooling to ink once it is all there. The spinner gives way to
/// "show original", because the original is one click away and stays.
///
/// No key: nobody asked for this. That is the lesson.
///
/// The seam: the read email lifts away and the next one rises, in Japanese,
/// drawn as the one the loop began on.
enum AutoTranslateScene: VignetteDrawing {
    /// English landed and cooled, the caption offering the original.
    static let posterTime: Double = 2.9

    private static let page = CGRect(x: 30, y: 10, width: 180, height: 96)
    private static let cell: CGFloat = 3.8
    private static let pitch: CGFloat = 4.7

    /// The Japanese: glyphs per line, with the gaps where its punctuation
    /// falls. The English: word widths per line.
    private static let japanese: [[Int]] = [[9, 12, 8], [14, 10, 6], [11, 9, 12], [7, 6]]
    private static let english: [[CGFloat]] = [
        [12, 20, 8, 16, 26, 10, 18, 14], [18, 10, 24, 14, 8, 22, 16, 12],
        [8, 26, 12, 18, 10, 20, 14, 16], [16, 22, 10, 18],
    ]
    private static let subjectJapanese: [Int] = [7, 6]
    private static let subjectEnglish: [CGFloat] = [22, 14, 30, 18]

    // The beats.
    private static let landFrom: Double = 0.7
    private static let lineStep: Double = 0.2
    private static let lineSpan: Double = 0.3
    private static let cool: Double = 2.05
    private static let leave: Double = 3.8
    private static let next: Double = 3.92

    static func draw(_ ctx: inout GraphicsContext, t: Double) {
        let ease = VignetteClock.ease
        let going = ease(t, leave, leave + 0.28)
        if going < 1 {
            drawPage(&ctx, t: t, live: true, dy: -10 * CGFloat(going), opacity: 1 - going)
        }
        if t >= next {
            let back = ease(t, next, next + 0.42)
            drawPage(&ctx, t: t, live: false, dy: 10 * CGFloat(1 - back), opacity: back)
        }
    }

    /// How far line `index` (0 the subject, then the body) has turned into
    /// English.
    private static func turned(_ index: Int, _ t: Double) -> Double {
        let from = landFrom + lineStep * Double(index)
        return VignetteClock.ramp(t, from, from + lineSpan)
    }

    private static func drawPage(_ ctx: inout GraphicsContext, t: Double, live: Bool, dy: CGFloat, opacity: Double) {
        guard opacity > 0.001 else { return }
        let ease = VignetteClock.ease
        var layer = ctx
        layer.opacity *= opacity
        layer.translateBy(x: 0, y: dy)
        let shape = Path(roundedRect: page, cornerRadius: 7, style: .continuous)
        layer.fill(shape, with: .color(VignettePalette.card))
        layer.fill(shape, with: .color(VignettePalette.faint))
        layer.stroke(shape, with: .color(VignettePalette.edge), lineWidth: 0.5)

        // Who wrote it, in their own name: the sender line stays as written.
        let x = page.minX + 12
        VignetteKit.avatar(&layer, center: CGPoint(x: x + 5, y: page.minY + 13), diameter: 10, tint: 2)
        glyphs(&layer, [5], x: x + 14, y: page.minY + 8.6, size: 3.2, pitch: 4, color: VignettePalette.ink.opacity(0.8))
        VignetteKit.bar(&layer, CGRect(x: x + 14, y: page.minY + 15, width: 30, height: 2.2), VignettePalette.dim)
        VignetteKit.bar(&layer, CGRect(x: page.maxX - 26, y: page.minY + 9.5, width: 14, height: 2.2), VignettePalette.dim)

        let cooled = live ? ease(t, cool, cool + 0.4) : 1
        // The subject, then the body.
        let subjectY = page.minY + 27
        line(&layer, japanese: subjectJapanese, english: subjectEnglish, x: x, y: subjectY, size: 4.4, height: 3.6,
             turn: live ? turned(0, t) : 0, cooled: cooled, ink: 0.9)
        for index in japanese.indices {
            line(&layer, japanese: japanese[index], english: english[index], x: x, y: page.minY + 50 + CGFloat(index) * 9,
                 size: cell, height: 2.6, turn: live ? turned(index + 1, t) : 0, cooled: cooled, ink: 0.62)
        }
        drawCaption(&layer, t: t, live: live, y: page.minY + 39)
    }

    /// One line, in Japanese or in English or on its way between: the
    /// glyphs fade as the words write on over them, left to right.
    private static func line(
        _ ctx: inout GraphicsContext, japanese: [Int], english: [CGFloat], x: CGFloat, y: CGFloat, size: CGFloat,
        height: CGFloat, turn: Double, cooled: Double, ink: Double
    ) {
        if turn < 1 {
            var original = ctx
            original.opacity *= 1 - VignetteClock.ease(turn, 0, 0.6)
            glyphs(&original, japanese, x: x, y: y - size / 2, size: size, pitch: size + 0.9,
                   color: VignettePalette.ink.opacity(ink))
        }
        guard turn > 0 else { return }
        let total = english.reduce(0, +) + CGFloat(english.count - 1) * 3
        let reach = total * CGFloat(turn)
        var wx = x
        for width in english {
            let shown = min(width, reach - (wx - x))
            if shown > 0 {
                let rect = CGRect(x: wx, y: y - height / 2, width: max(height, shown), height: height)
                VignetteKit.bar(&ctx, rect, VignettePalette.phosphor.opacity(1 - cooled))
                VignetteKit.bar(&ctx, rect, VignettePalette.ink.opacity(ink * cooled))
            }
            wx += width + 3
        }
        if turn < 1 {
            var hot = ctx
            hot.addFilter(.shadow(color: VignettePalette.phosphor.opacity(0.9), radius: 2.5))
            VignetteKit.bar(
                &hot, CGRect(x: x + reach - height, y: y - height / 2, width: height, height: height),
                VignettePalette.phosphorHot)
        }
    }

    /// Square glyphs in runs, a gap where the punctuation falls.
    private static func glyphs(
        _ ctx: inout GraphicsContext, _ runs: [Int], x: CGFloat, y: CGFloat, size: CGFloat, pitch: CGFloat, color: Color
    ) {
        var gx = x
        for run in runs {
            for _ in 0..<run {
                ctx.fill(
                    Path(roundedRect: CGRect(x: gx, y: y, width: size, height: size), cornerRadius: size * 0.22),
                    with: .color(color))
                gx += pitch
            }
            gx += pitch * 0.8
        }
    }

    /// The caption, after TranslationStrip: the translate mark (a glyph and
    /// a letter), the line, then a spinner while it works and "show
    /// original" once it is done. Micro type, faint, no fill and no edge.
    private static func drawCaption(_ ctx: inout GraphicsContext, t: Double, live: Bool, y: CGFloat) {
        let ease = VignetteClock.ease
        let ready = live ? ease(t, cool - 0.1, cool + 0.15) : 0
        let x = page.minX + 12
        ctx.fill(
            Path(roundedRect: CGRect(x: x, y: y - 1.6, width: 3.2, height: 3.2), cornerRadius: 0.7),
            with: .color(VignettePalette.dim))
        VignetteKit.bar(&ctx, CGRect(x: x + 4.2, y: y - 0.2, width: 2.4, height: 2), VignettePalette.dim)
        VignetteKit.bar(
            &ctx, CGRect(x: x + 10, y: y - 1.1, width: VignetteClock.mix(46, 40, ready), height: 2.2), VignettePalette.dim)
        let trailing = x + 10 + VignetteClock.mix(46, 40, ready) + 5
        // Seven whole turns a loop, so the spinner on the next email picks
        // up exactly where the first one's began.
        VignetteKit.arc(
            &ctx, center: CGPoint(x: trailing + 2, y: y), radius: 2.2, head: t * 7 / loop, sweep: 0.3,
            color: VignettePalette.ink.opacity(0.55), lineWidth: 0.8, opacity: 1 - ready)
        var action = ctx
        action.opacity *= ready
        action.fill(Path(ellipseIn: CGRect(x: trailing - 0.6, y: y - 0.6, width: 1.2, height: 1.2)), with: .color(VignettePalette.dim))
        VignetteKit.bar(&action, CGRect(x: trailing + 3.5, y: y - 1.1, width: 26, height: 2.2), VignettePalette.phosphorHot.opacity(0.9))
    }
}

// MARK: - search a year back

/// SEARCH A YEAR BACK: the search field, and under it the year of your mail
/// as a line: a year ago on the left, now on the right, and a pin where you
/// joined Passband. Search used to reach only the stretch since the pin,
/// lit faintly at rest. A query types itself in and the reach runs left
/// from now, past the pin and on to the year's start; each hit it passes
/// lights on the line, and its row drops into the results in the same
/// tint, newest first. Most of what it finds is from before the pin.
///
/// No key: search runs as you type.
///
/// The seam: the query is erased, the results and the reach go, and the
/// empty field and faint stretch the loop began on are left.
enum ArchiveSearchScene: VignetteDrawing {
    /// The reach at the year's start, every hit lit and listed.
    static let posterTime: Double = 2.6

    private static let field = CGRect(x: 10, y: 10, width: 220, height: 14)
    private static let lineY: CGFloat = 36
    private static let yearAgo: CGFloat = 20
    private static let now: CGFloat = 220
    private static let joined: CGFloat = 178

    /// The hits, newest first: where each sits on the year, and its row.
    private static let hits: [(x: CGFloat, tint: Int, sender: CGFloat, subject: CGFloat)] = [
        (204, 0, 26, 58), (188, 1, 20, 72), (146, 2, 30, 50), (98, 0, 24, 66), (44, 1, 28, 54),
    ]

    // The beats.
    private static let typeFrom: Double = 0.22
    private static let typeTo: Double = 0.66
    private static let reachFrom: Double = 0.74
    private static let reachTo: Double = 2.04
    private static let eraseFrom: Double = 3.95
    private static let eraseTo: Double = 4.3

    /// The reach's head, from now back to a year ago.
    private static func head(_ t: Double) -> CGFloat {
        VignetteClock.mix(now, yearAgo, VignetteClock.ease(t, reachFrom, reachTo))
    }

    /// When the reach passes a point on the line.
    private static func passes(_ x: CGFloat) -> Double {
        // Invert the smoothstep by bisection: the beat is wherever the head
        // actually is, so a hit never lights before the reach gets to it.
        var lo = reachFrom, hi = reachTo
        for _ in 0..<24 {
            let mid = (lo + hi) / 2
            if head(mid) > x { lo = mid } else { hi = mid }
        }
        return hi
    }

    static func draw(_ ctx: inout GraphicsContext, t: Double) {
        let ease = VignetteClock.ease
        let erased = ease(t, eraseFrom, eraseTo)
        drawField(&ctx, t: t, erased: erased)
        drawYear(&ctx, t: t, erased: erased)

        // The results, newest first, each dropping in as its hit lights.
        for (index, hit) in hits.enumerated() {
            let at = passes(hit.x)
            let shown = ease(t, at, at + 0.22) * (1 - ease(t, eraseFrom + 0.04 * Double(index), eraseTo + 0.04 * Double(index)))
            guard shown > 0.001 else { continue }
            var row = ctx
            row.opacity *= shown
            row.translateBy(x: 0, y: -4 * CGFloat(1 - ease(t, at, at + 0.22)))
            let y = 52 + CGFloat(index) * 11.5
            let mid = y + 4.5
            let lit = ease(t, at, at + 0.1) * (1 - ease(t, at + 0.35, at + 0.7))
            let rect = CGRect(x: 10, y: y - 1, width: 220, height: 11)
            let shape = Path(roundedRect: rect, cornerRadius: 3.5, style: .continuous)
            row.fill(shape, with: .color(VignettePalette.phosphor.opacity(0.1 * lit)))
            VignetteKit.avatar(&row, center: CGPoint(x: 18, y: mid), diameter: 6, tint: hit.tint)
            VignetteKit.bar(&row, CGRect(x: 26, y: mid - 1.4, width: hit.sender, height: 2.8), VignettePalette.ink.opacity(0.8))
            VignetteKit.bar(&row, CGRect(x: 64, y: mid - 1.4, width: hit.subject, height: 2.8), VignettePalette.ink.opacity(0.6))
            VignetteKit.bar(
                &row, CGRect(x: 64 + hit.subject + 4, y: mid - 1.2, width: 136 - hit.subject - 4, height: 2.4),
                VignettePalette.dim.opacity(0.7))
            // A date, wider the older it is: a day, then a month and a year.
            let date: CGFloat = hit.x > joined ? 12 : 20
            VignetteKit.bar(&row, CGRect(x: 230 - date - 2, y: mid - 1.2, width: date, height: 2.4), VignettePalette.dim)
        }
    }

    /// The field: a lens, and the query writing itself on.
    private static func drawField(_ ctx: inout GraphicsContext, t: Double, erased: Double) {
        let ease = VignetteClock.ease
        let shape = Path(roundedRect: field, cornerRadius: 7, style: .continuous)
        let focus = ease(t, typeFrom - 0.2, typeFrom) * (1 - ease(t, eraseTo, eraseTo + 0.3))
        ctx.fill(shape, with: .color(VignettePalette.faint))
        ctx.stroke(shape, with: .color(VignettePalette.edge.opacity(1 - focus)), lineWidth: 0.5)
        ctx.stroke(shape, with: .color(VignettePalette.phosphor.opacity(0.6 * focus)), lineWidth: 0.7)
        let lens = CGPoint(x: field.minX + 10, y: field.midY - 0.6)
        ctx.stroke(Path(ellipseIn: CGRect(x: lens.x - 2.6, y: lens.y - 2.6, width: 5.2, height: 5.2)),
                   with: .color(VignettePalette.dim), lineWidth: 0.8)
        var handle = Path()
        handle.move(to: CGPoint(x: lens.x + 1.9, y: lens.y + 1.9))
        handle.addLine(to: CGPoint(x: lens.x + 3.8, y: lens.y + 3.8))
        ctx.stroke(handle, with: .color(VignettePalette.dim), style: StrokeStyle(lineWidth: 0.8, lineCap: .round))

        let typed = VignetteClock.ramp(t, typeFrom, typeTo) * (1 - erased)
        guard typed > 0 else { return }
        let words: [CGFloat] = [22, 30]
        let full = words.reduce(0, +) + 3
        let reach = full * CGFloat(typed)
        var x = field.minX + 19
        for width in words {
            let shown = min(width, reach - (x - field.minX - 19))
            if shown > 0 {
                VignetteKit.bar(&ctx, CGRect(x: x, y: field.midY - 1.4, width: max(2.8, shown), height: 2.8),
                                VignettePalette.ink.opacity(0.85))
            }
            x += width + 3
        }
        if t < reachTo {
            ctx.fill(Path(CGRect(x: field.minX + 19 + reach + 1.2, y: field.midY - 3, width: 0.7, height: 6)),
                     with: .color(VignettePalette.phosphorHot))
        }
    }

    /// The year: a line from a year ago to now, months ticked, the pin where
    /// you joined, the old reach faint, and the new one running past it.
    private static func drawYear(_ ctx: inout GraphicsContext, t: Double, erased: Double) {
        let ease = VignetteClock.ease
        // Before you joined: there, but dashed, because it was out of reach.
        var before = Path()
        before.move(to: CGPoint(x: yearAgo, y: lineY))
        before.addLine(to: CGPoint(x: joined, y: lineY))
        ctx.stroke(before, with: .color(Color.white.opacity(0.18)), style: StrokeStyle(lineWidth: 0.8, lineCap: .round, dash: [2, 2.5]))
        var since = Path()
        since.move(to: CGPoint(x: joined, y: lineY))
        since.addLine(to: CGPoint(x: now, y: lineY))
        ctx.stroke(since, with: .color(VignettePalette.phosphor.opacity(0.4)), style: StrokeStyle(lineWidth: 1, lineCap: .round))
        for month in 0...12 {
            let x = yearAgo + (now - yearAgo) * CGFloat(month) / 12
            ctx.fill(Path(CGRect(x: x - 0.25, y: lineY + 2.5, width: 0.5, height: 2)), with: .color(Color.white.opacity(0.16)))
        }
        // Now, and the pin where you joined.
        ctx.fill(Path(ellipseIn: CGRect(x: now - 1.8, y: lineY - 1.8, width: 3.6, height: 3.6)), with: .color(VignettePalette.ink.opacity(0.8)))
        ctx.fill(Path(roundedRect: CGRect(x: joined - 0.4, y: lineY - 7, width: 0.8, height: 7), cornerRadius: 0.4),
                 with: .color(VignettePalette.ink.opacity(0.6)))
        ctx.fill(Path(ellipseIn: CGRect(x: joined - 2.2, y: lineY - 9.4, width: 4.4, height: 4.4)), with: .color(VignettePalette.ink.opacity(0.75)))
        VignetteKit.bar(&ctx, CGRect(x: joined - 9, y: lineY + 6.5, width: 18, height: 2), VignettePalette.dim)

        // The reach.
        let shown = ease(t, reachFrom - 0.05, reachFrom + 0.05) * (1 - erased)
        if shown > 0 {
            let x = head(t)
            var glow = ctx
            glow.opacity *= shown
            glow.addFilter(.shadow(color: VignettePalette.phosphor.opacity(0.8), radius: 3))
            var reach = Path()
            reach.move(to: CGPoint(x: now, y: lineY))
            reach.addLine(to: CGPoint(x: x, y: lineY))
            glow.stroke(reach, with: .color(VignettePalette.phosphor), style: StrokeStyle(lineWidth: 1.4, lineCap: .round))
            let moving = 1 - ease(t, reachTo - 0.1, reachTo + 0.2)
            glow.fill(Path(ellipseIn: CGRect(x: x - 1.8, y: lineY - 1.8, width: 3.6, height: 3.6)),
                      with: .color(VignettePalette.phosphorHot.opacity(moving)))
        }
        // The hits, each lit as the reach passes it.
        for hit in hits {
            let at = passes(hit.x)
            let lit = ease(t, at, at + 0.12) * (1 - erased)
            guard lit > 0 else { continue }
            var dot = ctx
            dot.opacity *= lit
            dot.addFilter(.shadow(color: VignettePalette.avatar[hit.tint].opacity(0.8), radius: 2.5))
            VignetteKit.avatar(&dot, center: CGPoint(x: hit.x, y: lineY), diameter: VignetteClock.mix(3, 5, lit), tint: hit.tint, opacity: 1)
        }
    }
}

// MARK: - auto-done

/// AUTO-DONE: your inbox, with an invitation at the top, and over it the day
/// going by: a strip of time sliding past a fixed now, the invitation's event
/// on it in the inviter's tint. The event comes up to now and passes under
/// it, lit while it is happening. Once it has ended, the invitation takes its
/// check, every email in its thread with it (the thread's edge under the
/// row), and slips out of the inbox on its own; the mail below closes up.
/// Later a new invitation lands on top, for the next event on the strip.
///
/// No key and no pointer anywhere, which is the whole point.
///
/// The seam: the strip has moved exactly one event along and every event is
/// drawn alike, and the invitation that lands is the one the loop began on.
enum AutoDoneScene: VignetteDrawing {
    /// The event just past now, the invitation checked and still there.
    static let posterTime: Double = 1.5

    private static let sender = 1
    private static let nowX: CGFloat = 120
    private static let strip = CGRect(x: 10, y: 10, width: 220, height: 18)
    private static let period: CGFloat = 110
    private static let eventX: CGFloat = 128
    private static let eventWidth: CGFloat = 36
    private static let laneTop: CGFloat = 38
    private static let pitch: CGFloat = 15

    // The beats.
    private static let scrollFrom: Double = 0.35
    private static let scrollTo: Double = 2.15
    /// When the event starts under now, and when it has ended: wherever the
    /// strip actually puts it, so the check never comes before the end.
    private static let began: Double = crosses(eventX - nowX)
    private static let ended: Double = crosses(eventX + eventWidth - nowX)
    private static let leaveFrom: Double = ended + 0.45
    private static let closeFrom: Double = leaveFrom + 0.28
    private static let landFrom: Double = 3.35

    private static func scrolled(_ t: Double) -> CGFloat {
        period * CGFloat(VignetteClock.ease(t, scrollFrom, scrollTo))
    }

    /// The moment the strip has scrolled `distance`, by bisection.
    private static func crosses(_ distance: CGFloat) -> Double {
        var lo = scrollFrom, hi = scrollTo
        for _ in 0..<24 {
            let mid = (lo + hi) / 2
            if scrolled(mid) < distance { lo = mid } else { hi = mid }
        }
        return hi
    }

    static func draw(_ ctx: inout GraphicsContext, t: Double) {
        let ease = VignetteClock.ease
        // While the invitation's event is under now, the invitation is lit.
        let during = ease(t, began, began + 0.12) * (1 - ease(t, ended, ended + 0.12))
        drawStrip(&ctx, offset: scrolled(t))
        drawLane(&ctx, t: t, during: during)
    }

    /// The day: hour ticks, the events on it, and now.
    private static func drawStrip(_ ctx: inout GraphicsContext, offset: CGFloat) {
        var band = ctx
        band.clipToLayer { mask in
            mask.fill(
                Path(strip),
                with: .linearGradient(
                    Gradient(stops: [
                        .init(color: .clear, location: 0), .init(color: .black, location: 0.12),
                        .init(color: .black, location: 0.88), .init(color: .clear, location: 1),
                    ]),
                    startPoint: CGPoint(x: strip.minX, y: 0), endPoint: CGPoint(x: strip.maxX, y: 0)))
        }
        // Ticks every hour, scrolling with the day.
        let tick: CGFloat = 11
        var x = strip.minX - offset.truncatingRemainder(dividingBy: tick)
        while x <= strip.maxX {
            band.fill(Path(CGRect(x: x - 0.25, y: strip.maxY - 3, width: 0.5, height: 3)), with: .color(Color.white.opacity(0.16)))
            x += tick
        }
        band.fill(Path(CGRect(x: strip.minX, y: strip.maxY - 0.25, width: strip.width, height: 0.5)), with: .color(VignettePalette.edge))
        // The events, one a period, all alike.
        for k in -1...2 {
            let left = eventX + period * CGFloat(k) - offset
            guard left < strip.maxX, left + eventWidth > strip.minX else { continue }
            let rect = CGRect(x: left, y: strip.minY + 3, width: eventWidth, height: 9)
            let under = left <= nowX && left + eventWidth >= nowX
            let past = left + eventWidth < nowX
            let shape = Path(roundedRect: rect, cornerRadius: 2.5, style: .continuous)
            band.fill(shape, with: .color(VignettePalette.avatar[sender].opacity(past ? 0.14 : under ? 0.5 : 0.32)))
            band.stroke(shape, with: .color(under ? VignettePalette.phosphor.opacity(0.8) : VignettePalette.edge), lineWidth: under ? 0.7 : 0.5)
            VignetteKit.bar(&band, CGRect(x: rect.minX + 4, y: rect.midY - 1, width: 18, height: 2),
                            VignettePalette.ink.opacity(past ? 0.3 : 0.75))
        }
        // Now: a fixed line, and the day goes by under it.
        var glow = ctx
        glow.addFilter(.shadow(color: VignettePalette.phosphor.opacity(0.8), radius: 2.5))
        glow.fill(Path(roundedRect: CGRect(x: nowX - 0.5, y: strip.minY, width: 1, height: strip.height + 1), cornerRadius: 0.5),
                  with: .color(VignettePalette.phosphor))
        glow.fill(Path(ellipseIn: CGRect(x: nowX - 2, y: strip.minY - 2, width: 4, height: 4)), with: .color(VignettePalette.phosphorHot))
    }

    private struct Row { let tint: Int; let sender: CGFloat; let subject: CGFloat }
    private static let rows: [Row] = [
        Row(tint: 0, sender: 24, subject: 54), Row(tint: 2, sender: 30, subject: 44),
        Row(tint: 0, sender: 20, subject: 62), Row(tint: 2, sender: 26, subject: 48),
        Row(tint: 0, sender: 22, subject: 58),
    ]

    /// The inbox: the invitation on top, mail under it.
    private static func drawLane(_ ctx: inout GraphicsContext, t: Double, during: Double) {
        let ease = VignetteClock.ease
        let check = ease(t, ended + 0.06, ended + 0.22)
        let leave = ease(t, leaveFrom, leaveFrom + 0.3)
        let close = ease(t, closeFrom, closeFrom + 0.36)
        let land = ease(t, landFrom, landFrom + 0.4)
        var lane = ctx
        VignetteKit.clipFading(&lane, CGRect(x: 0, y: laneTop - 4, width: 240, height: 120 - laneTop + 4), fade: 18)

        // Everything under the invitation moves up as it goes and down as the
        // next one lands: net, one row's worth of nothing.
        let shift = CGFloat(close - land)
        for (index, row) in rows.enumerated() {
            let y = laneTop + pitch * (CGFloat(index + 1) - shift)
            drawRow(&lane, y: y, tint: row.tint, sender: row.sender, subject: row.subject)
        }
        if leave < 1 {
            var going = lane
            going.opacity *= 1 - leave
            going.translateBy(x: 24 * CGFloat(leave), y: 0)
            drawInvite(&going, y: laneTop, lit: max(during, check), check: check)
        }
        if land > 0 {
            var coming = lane
            coming.opacity *= land
            coming.translateBy(x: 0, y: -6 * CGFloat(1 - land))
            drawInvite(&coming, y: laneTop, lit: 0, check: 0)
        }
    }

    /// The invitation: a thread, so a second edge shows under it, a calendar
    /// mark where the time would be.
    private static func drawInvite(_ ctx: inout GraphicsContext, y: CGFloat, lit: Double, check: Double) {
        let rect = CGRect(x: 10, y: y, width: 220, height: 12)
        let under = CGRect(x: 14, y: y + 2.6, width: 212, height: 12)
        ctx.fill(Path(roundedRect: under, cornerRadius: 4, style: .continuous), with: .color(VignettePalette.card.opacity(0.7)))
        ctx.stroke(Path(roundedRect: under, cornerRadius: 4, style: .continuous), with: .color(VignettePalette.edge), lineWidth: 0.5)
        let shape = Path(roundedRect: rect, cornerRadius: 4, style: .continuous)
        ctx.fill(shape, with: .color(VignettePalette.card))
        ctx.fill(shape, with: .color(VignettePalette.phosphor.opacity(0.08 * lit)))
        ctx.stroke(shape, with: .color(VignettePalette.edge), lineWidth: 0.5)
        if lit > 0 {
            ctx.stroke(shape, with: .color(VignettePalette.phosphor.opacity(0.85 * lit)), lineWidth: 0.7)
        }
        let mid = rect.midY
        let fade = VignetteClock.mix(1, 0.45, check)
        VignetteKit.avatar(&ctx, center: CGPoint(x: 19, y: mid), diameter: 6, tint: sender, opacity: 0.9 * fade)
        VignetteKit.bar(&ctx, CGRect(x: 27, y: mid - 1.4, width: 28, height: 2.8), VignettePalette.ink.opacity(0.85 * fade))
        VignetteKit.bar(&ctx, CGRect(x: 65, y: mid - 1.4, width: 52, height: 2.8), VignettePalette.ink.opacity(0.85 * fade))
        VignetteKit.bar(&ctx, CGRect(x: 121, y: mid - 1.2, width: 60, height: 2.4), VignettePalette.dim.opacity(fade))
        VignetteKit.calendarMark(&ctx, center: CGPoint(x: 206, y: mid), size: 6, color: VignettePalette.avatar[sender].opacity(0.9 * fade))
        VignetteKit.check(&ctx, center: CGPoint(x: 220, y: mid), size: 2.8, progress: check)
    }

    private static func drawRow(_ ctx: inout GraphicsContext, y: CGFloat, tint: Int, sender: CGFloat, subject: CGFloat) {
        guard y < 122 else { return }
        let mid = y + 6
        VignetteKit.avatar(&ctx, center: CGPoint(x: 19, y: mid), diameter: 6, tint: tint)
        VignetteKit.bar(&ctx, CGRect(x: 27, y: mid - 1.4, width: sender, height: 2.8), VignettePalette.ink.opacity(0.6))
        VignetteKit.bar(&ctx, CGRect(x: 65, y: mid - 1.4, width: subject, height: 2.8), VignettePalette.ink.opacity(0.6))
        VignetteKit.bar(&ctx, CGRect(x: 65 + subject + 4, y: mid - 1.2, width: 150 - subject - 4, height: 2.4), VignettePalette.dim.opacity(0.7))
        VignetteKit.bar(&ctx, CGRect(x: 217, y: mid - 1.2, width: 11, height: 2.4), VignettePalette.dim)
    }
}

// MARK: - parts the 0.0.16 scenes share

extension VignetteKit {
    /// The pointer: an arrow in the actor colour with an even light edge.
    /// Down, it shrinks a touch toward its tip and glows. Nobody's hand.
    static func pointer(_ ctx: inout GraphicsContext, tip: CGPoint, down: Double, opacity: Double) {
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

    /// An envelope: a mail account, as a mark. Solid or in outline, the way
    /// the welcome screen's two doors draw theirs (`envelope.fill` for Gmail,
    /// `envelope` for Outlook). Nobody's logo.
    static func envelopeMark(
        _ ctx: inout GraphicsContext, center: CGPoint, size: CGFloat, color: Color, filled: Bool
    ) {
        let rect = CGRect(x: center.x - size / 2, y: center.y - size * 0.36, width: size, height: size * 0.72)
        let body = Path(roundedRect: rect, cornerRadius: size * 0.12, style: .continuous)
        var flap = Path()
        flap.move(to: CGPoint(x: rect.minX + size * 0.1, y: rect.minY + size * 0.12))
        flap.addLine(to: CGPoint(x: center.x, y: rect.minY + rect.height * 0.56))
        flap.addLine(to: CGPoint(x: rect.maxX - size * 0.1, y: rect.minY + size * 0.12))
        let line = StrokeStyle(lineWidth: max(0.5, size * 0.075), lineCap: .round, lineJoin: .round)
        if filled {
            ctx.fill(body, with: .color(color))
            ctx.stroke(flap, with: .color(VignettePalette.groundBottom.opacity(0.75)), style: line)
        } else {
            ctx.stroke(body, with: .color(color), style: line)
            ctx.stroke(flap, with: .color(color), style: line)
        }
    }

    /// A ticket: a stub with a bite out of either end and a perforation.
    static func ticketMark(_ ctx: inout GraphicsContext, center: CGPoint, size: CGFloat, color: Color) {
        let rect = CGRect(x: center.x - size / 2, y: center.y - size * 0.32, width: size, height: size * 0.64)
        ctx.stroke(ticketPath(rect), with: .color(color), style: StrokeStyle(lineWidth: max(0.5, size * 0.08), lineJoin: .round))
        var tear = Path()
        tear.move(to: CGPoint(x: rect.minX + size * 0.66, y: rect.minY + size * 0.12))
        tear.addLine(to: CGPoint(x: rect.minX + size * 0.66, y: rect.maxY - size * 0.12))
        ctx.stroke(tear, with: .color(color), style: StrokeStyle(lineWidth: max(0.4, size * 0.06), dash: [size * 0.08, size * 0.08]))
    }

    /// A ticket's outline in `rect`: square shoulders, a half-round bite out
    /// of the middle of each end.
    static func ticketPath(_ rect: CGRect) -> Path {
        let r = rect.height * 0.18
        let c = min(rect.height * 0.14, 3)
        var path = Path()
        path.move(to: CGPoint(x: rect.minX + c, y: rect.minY))
        path.addLine(to: CGPoint(x: rect.maxX - c, y: rect.minY))
        path.addQuadCurve(to: CGPoint(x: rect.maxX, y: rect.minY + c), control: CGPoint(x: rect.maxX, y: rect.minY))
        path.addLine(to: CGPoint(x: rect.maxX, y: rect.midY - r))
        path.addArc(
            center: CGPoint(x: rect.maxX, y: rect.midY), radius: r, startAngle: .degrees(-90), endAngle: .degrees(90),
            clockwise: true)
        path.addLine(to: CGPoint(x: rect.maxX, y: rect.maxY - c))
        path.addQuadCurve(to: CGPoint(x: rect.maxX - c, y: rect.maxY), control: CGPoint(x: rect.maxX, y: rect.maxY))
        path.addLine(to: CGPoint(x: rect.minX + c, y: rect.maxY))
        path.addQuadCurve(to: CGPoint(x: rect.minX, y: rect.maxY - c), control: CGPoint(x: rect.minX, y: rect.maxY))
        path.addLine(to: CGPoint(x: rect.minX, y: rect.midY + r))
        path.addArc(
            center: CGPoint(x: rect.minX, y: rect.midY), radius: r, startAngle: .degrees(90), endAngle: .degrees(-90),
            clockwise: true)
        path.addLine(to: CGPoint(x: rect.minX, y: rect.minY + c))
        path.addQuadCurve(to: CGPoint(x: rect.minX + c, y: rect.minY), control: CGPoint(x: rect.minX, y: rect.minY))
        path.closeSubpath()
        return path
    }

    /// A place pin: a drop with a hole in it.
    static func pinMark(_ ctx: inout GraphicsContext, tip: CGPoint, size: CGFloat, color: Color, filled: Bool = false) {
        let r = size * 0.36
        let head = CGPoint(x: tip.x, y: tip.y - size + r)
        var drop = Path()
        drop.move(to: tip)
        drop.addCurve(
            to: CGPoint(x: head.x - r, y: head.y), control1: CGPoint(x: tip.x - r * 0.35, y: tip.y - size * 0.3),
            control2: CGPoint(x: head.x - r, y: head.y + r * 0.7))
        drop.addArc(center: head, radius: r, startAngle: .degrees(180), endAngle: .degrees(0), clockwise: false)
        drop.addCurve(
            to: tip, control1: CGPoint(x: head.x + r, y: head.y + r * 0.7),
            control2: CGPoint(x: tip.x + r * 0.35, y: tip.y - size * 0.3))
        drop.closeSubpath()
        if filled {
            ctx.fill(drop, with: .color(color))
            ctx.fill(
                Path(ellipseIn: CGRect(x: head.x - r * 0.4, y: head.y - r * 0.4, width: r * 0.8, height: r * 0.8)),
                with: .color(VignettePalette.groundBottom.opacity(0.8)))
        } else {
            ctx.stroke(drop, with: .color(color), style: StrokeStyle(lineWidth: max(0.5, size * 0.1), lineJoin: .round))
        }
    }

    /// A point `p` of the way along a quadratic curve.
    static func quad(_ a: CGPoint, _ control: CGPoint, _ b: CGPoint, _ p: Double) -> CGPoint {
        let s = CGFloat(p), u = 1 - s
        return CGPoint(
            x: u * u * a.x + 2 * u * s * control.x + s * s * b.x,
            y: u * u * a.y + 2 * u * s * control.y + s * s * b.y)
    }
}

// MARK: - calendar

/// CALENDAR: the month, the way the page draws it: seven columns of days,
/// each listing its day top-down as chips in its calendar's colour, a trip as
/// a bar across the days it covers, today's date in the actor colour. Beside
/// it, mail. A concert's confirmation is on top, its photo in the corner and
/// a calendar mark saying there is an event in it.
///
/// The email lights, and its event lifts out of it: a GHOST, outlined and
/// striped the way the page draws an event it found in your mail, gliding
/// along its own trail to its day. It lands, takes the day's selection and
/// fills solid, and its card opens over the mail it came from: the picture
/// strip on top (the photo from the email, the map of where), then its name,
/// its time, its place and who is going.
///
/// No key and no pointer: nobody typed this event in. That is the magic.
///
/// The seam: the card closes, the email cools, and the month pages to the
/// next one, drawn as the month the loop began on.
enum CalendarScene: VignetteDrawing {
    /// The event on its day, its card open with the picture strip.
    static let posterTime: Double = 2.75

    private static let column = CGRect(x: 10, y: 22, width: 62, height: 88)
    private static let grid = CGRect(x: 82, y: 30, width: 148, height: 80)
    private static let cellW: CGFloat = 148 / 7
    private static let cellH: CGFloat = 16

    private struct Chip { let row: Int; let col: Int; let slot: Int; let tint: Int }
    /// The month's own events: placed the way a real month falls, a busy
    /// day here and a quiet run there, not a pattern.
    private static let chips: [Chip] = [
        Chip(row: 0, col: 1, slot: 0, tint: 2), Chip(row: 0, col: 4, slot: 0, tint: 1),
        Chip(row: 0, col: 4, slot: 1, tint: 2), Chip(row: 1, col: 0, slot: 0, tint: 1),
        Chip(row: 1, col: 5, slot: 1, tint: 2), Chip(row: 1, col: 6, slot: 0, tint: 1),
        Chip(row: 2, col: 2, slot: 0, tint: 2), Chip(row: 2, col: 3, slot: 0, tint: 1),
        Chip(row: 2, col: 3, slot: 1, tint: 2), Chip(row: 2, col: 6, slot: 0, tint: 2),
        Chip(row: 3, col: 1, slot: 0, tint: 1), Chip(row: 3, col: 4, slot: 0, tint: 2),
        Chip(row: 4, col: 0, slot: 0, tint: 2), Chip(row: 4, col: 2, slot: 0, tint: 2),
        Chip(row: 4, col: 5, slot: 0, tint: 1), Chip(row: 4, col: 5, slot: 1, tint: 2),
    ]
    /// A stay across three days, as one bar.
    private static let trip = (row: 1, from: 1, to: 3, tint: 2)
    private static let today = (row: 1, col: 5)
    /// Where the concert lands: the second row of a day that has one thing.
    private static let target = (row: 3, col: 4, slot: 1)
    private static let photoTint = 2

    // The beats.
    private static let lightFrom: Double = 0.3
    private static let depart: Double = 0.7
    private static let land: Double = 1.5
    private static let cardIn: Double = 1.9
    private static let cardOut: Double = 3.6
    private static let pageFrom: Double = 3.95
    private static let pageTo: Double = 4.5

    private static func cell(_ row: Int, _ col: Int) -> CGRect {
        CGRect(x: grid.minX + CGFloat(col) * cellW, y: grid.minY + CGFloat(row) * cellH, width: cellW, height: cellH)
    }

    private static func chipRect(_ row: Int, _ col: Int, _ slot: Int) -> CGRect {
        let c = cell(row, col)
        return CGRect(x: c.minX + 1.6, y: c.minY + 5.4 + CGFloat(slot) * 5.2, width: c.width - 3.2, height: 4.4)
    }

    /// The calendar mark in the corner of the concert's email: where the
    /// ghost lifts from.
    private static let source = CGPoint(x: column.maxX - 8, y: column.minY + 19)

    static func draw(_ ctx: inout GraphicsContext, t: Double) {
        let ease = VignetteClock.ease
        let card = ease(t, cardIn, cardIn + 0.32) * (1 - ease(t, cardOut, cardOut + 0.3))
        drawColumn(&ctx, t: t, under: card)
        drawMonth(&ctx, t: t)
        drawFlight(&ctx, t: t)
        drawCard(&ctx, t: t, shown: card)
    }

    // MARK: the mail

    private static func drawColumn(_ ctx: inout GraphicsContext, t: Double, under: Double) {
        let ease = VignetteClock.ease
        var layer = ctx
        layer.opacity *= 1 - 0.7 * under
        VignetteKit.bar(
            &layer, CGRect(x: column.minX + 1, y: 14.2, width: 22, height: 3),
            VignettePalette.ink.opacity(0.55 * (1 - under)))
        let lit = ease(t, lightFrom, lightFrom + 0.2) * (1 - ease(t, land + 0.1, land + 0.5))
        let designs: [(tint: Int, title: CGFloat, lines: (CGFloat, CGFloat))] = [
            (photoTint, 26, (34, 22)), (0, 30, (26, 32)), (1, 22, (34, 18)),
        ]
        for (index, d) in designs.enumerated() {
            let rect = CGRect(x: column.minX, y: column.minY + CGFloat(index) * 30, width: column.width, height: 26)
            let shape = Path(roundedRect: rect, cornerRadius: 6, style: .continuous)
            layer.fill(shape, with: .color(VignettePalette.card))
            layer.fill(shape, with: .color(VignettePalette.faint))
            layer.stroke(shape, with: .color(VignettePalette.edge), lineWidth: 0.5)
            let hero = CGRect(x: rect.minX + 4, y: rect.minY + 4, width: 13, height: 13)
            if index == 0 {
                if lit > 0 {
                    layer.stroke(shape, with: .color(VignettePalette.phosphor.opacity(0.85 * lit)), lineWidth: 0.7)
                }
                photo(&layer, hero, corner: 3)
            } else {
                layer.fill(
                    Path(roundedRect: hero, cornerRadius: 3, style: .continuous),
                    with: .color(VignettePalette.avatar[d.tint].opacity(0.45)))
            }
            let x = rect.minX + 21
            VignetteKit.bar(&layer, CGRect(x: x, y: rect.minY + 5.5, width: d.title, height: 2.6), VignettePalette.ink.opacity(0.8))
            VignetteKit.bar(&layer, CGRect(x: x, y: rect.minY + 11.5, width: d.lines.0, height: 2.2), VignettePalette.dim)
            VignetteKit.bar(&layer, CGRect(x: x, y: rect.minY + 16.5, width: d.lines.1, height: 2.2), VignettePalette.dim)
        }
        VignetteKit.calendarMark(
            &layer, center: source, size: 6,
            color: VignetteClock.mix(0, 1, lit) > 0.5 ? VignettePalette.phosphorHot : VignettePalette.ink.opacity(0.6))
    }

    /// The email's photo: a warm sky, a sun and a ridge. A picture, not a
    /// colour swatch.
    static func photo(_ ctx: inout GraphicsContext, _ rect: CGRect, corner: CGFloat) {
        var layer = ctx
        let shape = Path(roundedRect: rect, cornerRadius: corner, style: .continuous)
        layer.clip(to: shape)
        layer.fill(
            shape,
            with: .linearGradient(
                Gradient(colors: [VignettePalette.avatar[1].opacity(0.75), VignettePalette.avatar[photoTint].opacity(0.7)]),
                startPoint: CGPoint(x: rect.midX, y: rect.minY), endPoint: CGPoint(x: rect.midX, y: rect.maxY)))
        let r = rect.height * 0.17
        layer.fill(
            Path(ellipseIn: CGRect(x: rect.minX + rect.width * 0.62 - r, y: rect.minY + rect.height * 0.34 - r, width: 2 * r, height: 2 * r)),
            with: .color(Color.white.opacity(0.7)))
        var ridge = Path()
        ridge.move(to: CGPoint(x: rect.minX, y: rect.maxY))
        ridge.addLine(to: CGPoint(x: rect.minX, y: rect.minY + rect.height * 0.72))
        ridge.addLine(to: CGPoint(x: rect.minX + rect.width * 0.32, y: rect.minY + rect.height * 0.5))
        ridge.addLine(to: CGPoint(x: rect.minX + rect.width * 0.58, y: rect.minY + rect.height * 0.74))
        ridge.addLine(to: CGPoint(x: rect.minX + rect.width * 0.8, y: rect.minY + rect.height * 0.6))
        ridge.addLine(to: CGPoint(x: rect.maxX, y: rect.minY + rect.height * 0.7))
        ridge.addLine(to: CGPoint(x: rect.maxX, y: rect.maxY))
        ridge.closeSubpath()
        layer.fill(ridge, with: .color(VignettePalette.groundTop.opacity(0.75)))
    }

    // MARK: the month

    private static func drawMonth(_ ctx: inout GraphicsContext, t: Double) {
        let ease = VignetteClock.ease
        // The bar over the grid: the month's name, and Month / Week with
        // Month on.
        let paged = CGFloat(ease(t, pageFrom, pageTo))
        var head = ctx
        head.clip(to: Path(CGRect(x: grid.minX, y: 8, width: 100, height: 14)))
        for k in 0...1 {
            VignetteKit.bar(
                &head, CGRect(x: grid.minX + 1 + 100 * (CGFloat(k) - paged), y: 13.8, width: 34, height: 3.4),
                VignettePalette.ink.opacity(0.8))
        }
        let toggle = CGRect(x: grid.maxX - 36, y: 10.5, width: 36, height: 10)
        ctx.fill(Path(roundedRect: toggle, cornerRadius: 5), with: .color(VignettePalette.faint))
        ctx.stroke(Path(roundedRect: toggle, cornerRadius: 5), with: .color(VignettePalette.edge), lineWidth: 0.5)
        let on = CGRect(x: toggle.minX + 1, y: toggle.minY + 1, width: 17, height: 8)
        ctx.fill(Path(roundedRect: on, cornerRadius: 4), with: .color(Color.white.opacity(0.12)))
        VignetteKit.bar(&ctx, CGRect(x: on.minX + 4, y: on.midY - 1, width: 9, height: 2), VignettePalette.ink.opacity(0.85))
        VignetteKit.bar(&ctx, CGRect(x: toggle.minX + 23, y: on.midY - 1, width: 8, height: 2), VignettePalette.dim)

        // Weekday initials, then the grid's hairlines. Neither moves when
        // the month pages: the days line up under them either way.
        for col in 0..<7 {
            VignetteKit.bar(
                &ctx, CGRect(x: grid.minX + CGFloat(col) * cellW + cellW / 2 - 2.5, y: grid.minY - 5, width: 5, height: 1.8),
                VignettePalette.dim.opacity(0.8))
        }
        for row in 0...5 {
            ctx.fill(
                Path(CGRect(x: grid.minX, y: grid.minY + CGFloat(row) * cellH - 0.25, width: grid.width, height: 0.5)),
                with: .color(VignettePalette.edge))
        }
        for col in 1..<7 {
            ctx.fill(
                Path(CGRect(x: grid.minX + CGFloat(col) * cellW - 0.25, y: grid.minY, width: 0.5, height: grid.height)),
                with: .color(VignettePalette.edge))
        }

        var days = ctx
        days.clip(to: Path(grid))
        drawDays(&days, t: t, dx: -grid.width * paged, live: true)
        if paged > 0 { drawDays(&days, t: t, dx: grid.width * (1 - paged), live: false) }
    }

    /// One month's contents: dates, today, the trip, the chips, and (for the
    /// live month) the concert once it has landed and the day's selection.
    private static func drawDays(_ ctx: inout GraphicsContext, t: Double, dx: CGFloat, live: Bool) {
        let ease = VignetteClock.ease
        var layer = ctx
        layer.translateBy(x: dx, y: 0)
        for row in 0..<5 {
            for col in 0..<7 {
                let c = cell(row, col)
                if row == today.row && col == today.col {
                    var glow = layer
                    glow.addFilter(.shadow(color: VignettePalette.phosphor.opacity(0.7), radius: 2))
                    glow.fill(Path(ellipseIn: CGRect(x: c.minX + 1.4, y: c.minY + 1.2, width: 4, height: 4)), with: .color(VignettePalette.phosphor))
                } else {
                    VignetteKit.bar(&layer, CGRect(x: c.minX + 2, y: c.minY + 2.4, width: col % 2 == 0 ? 3 : 4, height: 1.6), VignettePalette.dim)
                }
            }
        }
        // The stay: one bar across its days, flat where it runs on.
        let from = chipRect(trip.row, trip.from, 0), to = chipRect(trip.row, trip.to, 0)
        let stay = CGRect(x: from.minX, y: from.minY, width: to.maxX - from.minX, height: from.height)
        layer.fill(Path(roundedRect: stay, cornerRadius: 1.6, style: .continuous), with: .color(VignettePalette.avatar[trip.tint].opacity(0.4)))
        VignetteKit.bar(&layer, CGRect(x: stay.minX + 2.5, y: stay.midY - 0.7, width: 20, height: 1.4), VignettePalette.ink.opacity(0.75))
        for chip in chips {
            let rect = chipRect(chip.row, chip.col, chip.slot)
            layer.fill(Path(roundedRect: rect, cornerRadius: 1.6, style: .continuous), with: .color(VignettePalette.avatar[chip.tint].opacity(0.36)))
            VignetteKit.bar(
                &layer, CGRect(x: rect.minX + 1.6, y: rect.midY - 0.7, width: CGFloat(6 + (chip.row * 3 + chip.col) % 6), height: 1.4),
                VignettePalette.ink.opacity(0.7))
        }
        guard live else { return }
        let landed = ease(t, land - 0.02, land + 0.02)
        if landed > 0 {
            ghost(&layer, chipRect(target.row, target.col, target.slot), solid: ease(t, land + 0.08, land + 0.36), glow: 1 - ease(t, land + 0.3, land + 0.9))
        }
        // The day's selection: an even edge in ink, all the way round.
        let selected = ease(t, land + 0.05, land + 0.3) * (1 - ease(t, cardOut, cardOut + 0.3))
        if selected > 0 {
            let c = cell(target.row, target.col).insetBy(dx: 0.5, dy: 0.5)
            layer.stroke(
                Path(roundedRect: c, cornerRadius: 2, style: .continuous),
                with: .color(VignettePalette.ink.opacity(0.75 * selected)), lineWidth: 0.8)
        }
    }

    /// The concert as the page draws it: outlined and striped while it is
    /// a ghost (found in mail), solid once it is on the calendar.
    private static func ghost(_ ctx: inout GraphicsContext, _ rect: CGRect, solid: Double, glow: Double) {
        let shape = Path(roundedRect: rect, cornerRadius: 1.6, style: .continuous)
        var layer = ctx
        if glow > 0 { layer.addFilter(.shadow(color: VignettePalette.phosphor.opacity(0.85 * glow), radius: 3)) }
        layer.fill(shape, with: .color(VignettePalette.phosphor.opacity(0.14 + 0.66 * solid)))
        var stripes = ctx
        stripes.clip(to: shape)
        stripes.opacity *= 1 - solid
        var x = rect.minX - rect.height
        while x < rect.maxX {
            var line = Path()
            line.move(to: CGPoint(x: x, y: rect.maxY))
            line.addLine(to: CGPoint(x: x + rect.height, y: rect.minY))
            stripes.stroke(line, with: .color(VignettePalette.phosphor.opacity(0.7)), lineWidth: 0.6)
            x += 2.2
        }
        ctx.stroke(shape, with: .color(VignettePalette.phosphorHot.opacity(0.9 - 0.5 * solid)), lineWidth: 0.6)
        VignetteKit.bar(
            &ctx, CGRect(x: rect.minX + 1.6, y: rect.midY - 0.7, width: 10, height: 1.4),
            Color.white.opacity(0.4 + 0.5 * solid))
    }

    // MARK: the flight

    private static func drawFlight(_ ctx: inout GraphicsContext, t: Double) {
        let ease = VignetteClock.ease
        let to = chipRect(target.row, target.col, target.slot)
        let end = CGPoint(x: to.midX, y: to.midY)
        let control = CGPoint(x: (source.x + end.x) / 2 + 6, y: min(source.y, end.y) - 8)
        let p = ease(t, depart, land)

        // The trail, drawn on behind it and fading once it has landed.
        let trail = ease(t, depart - 0.05, depart + 0.05) * (1 - ease(t, land + 0.1, land + 0.6))
        if trail > 0, p > 0 {
            var path = Path()
            path.move(to: source)
            let steps = 40
            for i in 1...steps {
                path.addLine(to: VignetteKit.quad(source, control, end, p * Double(i) / Double(steps)))
            }
            var layer = ctx
            layer.opacity *= trail
            layer.stroke(
                path, with: .color(VignettePalette.phosphor.opacity(0.55)),
                style: StrokeStyle(lineWidth: 0.7, lineCap: .round, dash: [1.6, 2.2]))
        }
        // The ghost in the air: in as it lifts, gone the instant it lands
        // (the month's own copy takes over in the same place).
        guard t > depart - 0.1, t < land else { return }
        let at = VignetteKit.quad(source, control, end, p)
        let rect = CGRect(x: at.x - to.width / 2, y: at.y - to.height / 2, width: to.width, height: to.height)
        var layer = ctx
        layer.opacity *= ease(t, depart - 0.1, depart + 0.1)
        ghost(&layer, rect, solid: 0, glow: 1)
    }

    // MARK: the event's card

    /// The event, open: the picture strip, its name, when, where and who.
    private static func drawCard(_ ctx: inout GraphicsContext, t: Double, shown: Double) {
        guard shown > 0.001 else { return }
        let ease = VignetteClock.ease
        let rect = CGRect(x: 10, y: 14, width: 64, height: 96)
        var layer = ctx
        layer.opacity *= shown
        layer.translateBy(x: 0, y: 8 * CGFloat(1 - ease(t, cardIn, cardIn + 0.32)))
        let shape = Path(roundedRect: rect, cornerRadius: 8, style: .continuous)
        var lifted = layer
        lifted.addFilter(.shadow(color: Color.black.opacity(0.5), radius: 6))
        lifted.fill(shape, with: .color(VignettePalette.cap))
        layer.stroke(shape, with: .color(Color.white.opacity(0.14)), lineWidth: 0.5)

        // The picture strip: the email's photo, and the map of where.
        let photoRect = CGRect(x: rect.minX + 5, y: rect.minY + 5, width: 26, height: 24)
        photo(&layer, photoRect, corner: 4)
        let map = CGRect(x: rect.minX + 33, y: rect.minY + 5, width: 26, height: 24)
        let mapShape = Path(roundedRect: map, cornerRadius: 4, style: .continuous)
        layer.fill(mapShape, with: .color(Color(hex: 0x1D2A3C)))
        var streets = layer
        streets.clip(to: mapShape)
        let lines: [(CGPoint, CGPoint, CGFloat)] = [
            (CGPoint(x: map.minX, y: map.minY + 8), CGPoint(x: map.maxX, y: map.minY + 5), 1.2),
            (CGPoint(x: map.minX, y: map.minY + 18), CGPoint(x: map.maxX, y: map.minY + 20), 0.8),
            (CGPoint(x: map.minX + 9, y: map.minY), CGPoint(x: map.minX + 7, y: map.maxY), 0.8),
            (CGPoint(x: map.minX + 19, y: map.minY), CGPoint(x: map.minX + 22, y: map.maxY), 1.2),
        ]
        for line in lines {
            var path = Path()
            path.move(to: line.0)
            path.addLine(to: line.1)
            streets.stroke(path, with: .color(Color.white.opacity(0.13)), lineWidth: line.2)
        }
        layer.stroke(mapShape, with: .color(VignettePalette.edge), lineWidth: 0.5)
        var pin = layer
        pin.addFilter(.shadow(color: VignettePalette.phosphor.opacity(0.8), radius: 2))
        VignetteKit.pinMark(&pin, tip: CGPoint(x: map.midX + 1, y: map.midY + 4), size: 8, color: VignettePalette.phosphor, filled: true)

        // Its name, when, where, and who is going.
        let x = rect.minX + 6
        VignetteKit.bar(&layer, CGRect(x: x, y: rect.minY + 36, width: 42, height: 3.6), VignettePalette.ink.opacity(0.9))
        VignetteKit.ring(
            &layer, center: CGPoint(x: x + 2.2, y: rect.minY + 47), radius: 2, progress: 1,
            color: VignettePalette.dim, lineWidth: 0.6)
        VignetteKit.bar(&layer, CGRect(x: x + 7, y: rect.minY + 46, width: 30, height: 2.2), VignettePalette.ink.opacity(0.6))
        VignetteKit.pinMark(&layer, tip: CGPoint(x: x + 2.2, y: rect.minY + 57.5), size: 4.6, color: VignettePalette.dim)
        VignetteKit.bar(&layer, CGRect(x: x + 7, y: rect.minY + 54, width: 38, height: 2.2), VignettePalette.ink.opacity(0.6))
        for index in 0..<3 {
            let c = CGPoint(x: x + 4 + CGFloat(index) * 5.4, y: rect.minY + 68)
            layer.fill(Path(ellipseIn: CGRect(x: c.x - 4, y: c.y - 4, width: 8, height: 8)), with: .color(VignettePalette.cap))
            VignetteKit.avatar(&layer, center: c, diameter: 7, tint: index)
        }
        VignetteKit.bar(&layer, CGRect(x: x + 22, y: rect.minY + 67, width: 18, height: 2.2), VignettePalette.dim)
        // The note, from the email.
        VignetteKit.bar(&layer, CGRect(x: x, y: rect.minY + 79, width: 50, height: 2.2), VignettePalette.dim.opacity(0.8))
        VignetteKit.bar(&layer, CGRect(x: x, y: rect.minY + 84.5, width: 34, height: 2.2), VignettePalette.dim.opacity(0.8))
    }
}

// MARK: - reminders that know the event

/// REMINDERS THAT KNOW THE EVENT: the day as a strip sliding past a fixed
/// now, a concert coming up on it. The model's mark glints over the event and
/// three lead times tick in before it (an hour, half an hour, ten minutes);
/// it picks one, which lights, and the stretch from there to the doors lights
/// with it. The day runs on until now reaches the pick, and the banner drops
/// in: the line the model wrote writing itself on, under it the time and the
/// place as the app sets them ("8pm · The Fillmore"), and the helper, a
/// Tickets button. A pointer presses it and the email that holds the tickets
/// opens, the ticket itself front and centre.
///
/// The one press is the helper's: the reminder chose its own moment.
///
/// The seam: the email closes, and the strip runs on exactly one event's
/// period, so the next concert sits where this one began, its lead time not
/// yet chosen. Every event on the strip is drawn alike.
enum EventReminderScene: VignetteDrawing {
    /// The banner down, the line written, Tickets about to be pressed.
    static let posterTime: Double = 2.6

    private static let strip = CGRect(x: 10, y: 50, width: 220, height: 54)
    private static let nowX: CGFloat = 56
    /// An hour, in units.
    private static let hour: CGFloat = 40
    private static let leads: [CGFloat] = [40, 20, 40.0 / 6]
    private static let picked = 1
    private static let eventStart: CGFloat = 142
    private static let eventWidth: CGFloat = 66
    private static let period: CGFloat = 176
    private static let concert = 1
    private static let banner = CGRect(x: 122, y: 10, width: 108, height: 32)
    private static let button = CGRect(x: 144, y: 29.5, width: 32, height: 9)
    private static let mail = CGRect(x: 22, y: 12, width: 112, height: 96)

    // The beats.
    private static let glint: Double = 0.25
    private static let ticks: [Double] = [0.35, 0.45, 0.55]
    private static let pick: Double = 0.82
    private static let runFrom: Double = 1.15
    private static let runTo: Double = 1.8
    private static let bannerIn: Double = 1.86
    private static let press: Double = 2.75
    private static let open: Double = 2.9
    private static let close: Double = 3.7
    private static let onFrom: Double = 3.95
    private static let onTo: Double = 4.6

    /// How far the day has slid: up to the pick, then on to one period.
    private static func scrolled(_ t: Double) -> CGFloat {
        let first = eventStart - leads[picked] - nowX
        return first * CGFloat(VignetteClock.ease(t, runFrom, runTo))
            + (period - first) * CGFloat(VignetteClock.ease(t, onFrom, onTo))
    }

    static func draw(_ ctx: inout GraphicsContext, t: Double) {
        let ease = VignetteClock.ease
        let reading = ease(t, open, open + 0.3) * (1 - ease(t, close, close + 0.25))
        var day = ctx
        day.opacity *= 1 - 0.6 * reading
        // The day's heading: today, and how much is on it.
        VignetteKit.bar(&day, CGRect(x: 11, y: 14.2, width: 28, height: 3), VignettePalette.ink.opacity(0.55))
        VignetteKit.bar(&day, CGRect(x: 43, y: 14.5, width: 16, height: 2.4), VignettePalette.dim)
        drawStrip(&day, t: t, offset: scrolled(t))
        drawBanner(&ctx, t: t)
        drawMail(&ctx, t: t, shown: reading)

        // The pointer, in for the one press and gone once the email opens.
        let arrive = ease(t, press - 0.6, press - 0.4) * (1 - ease(t, open, open + 0.15))
        if arrive > 0 {
            let from = CGPoint(x: 204, y: 96), to = CGPoint(x: button.midX + 6, y: button.midY + 1.5)
            let move = ease(t, press - 0.5, press - 0.06)
            VignetteKit.pointer(
                &ctx, tip: CGPoint(x: VignetteClock.mix(from.x, to.x, move), y: VignetteClock.mix(from.y, to.y, move)),
                down: VignetteClock.press(t, at: press), opacity: arrive)
        }
    }

    // MARK: the day

    private static func drawStrip(_ ctx: inout GraphicsContext, t: Double, offset: CGFloat) {
        let ease = VignetteClock.ease
        var band = ctx
        band.clipToLayer { mask in
            mask.fill(
                Path(strip.insetBy(dx: 0, dy: -12)),
                with: .linearGradient(
                    Gradient(stops: [
                        .init(color: .clear, location: 0), .init(color: .black, location: 0.12),
                        .init(color: .black, location: 0.88), .init(color: .clear, location: 1),
                    ]),
                    startPoint: CGPoint(x: strip.minX, y: 0), endPoint: CGPoint(x: strip.maxX, y: 0)))
        }
        let base = strip.maxY - 6
        // Quarter hours, the hours taller.
        let quarter = hour / 4
        var k = Int(((strip.minX + offset) / quarter).rounded(.down))
        while true {
            let x = CGFloat(k) * quarter - offset
            if x > strip.maxX { break }
            let tall: CGFloat = k % 4 == 0 ? 4 : 2
            band.fill(Path(CGRect(x: x - 0.25, y: base - tall, width: 0.5, height: tall)), with: .color(Color.white.opacity(k % 4 == 0 ? 0.2 : 0.1)))
            k += 1
        }
        band.fill(Path(CGRect(x: strip.minX, y: base - 0.25, width: strip.width, height: 0.5)), with: .color(VignettePalette.edge))

        // The events, one a period, all alike.
        for n in -1...1 {
            let left = eventStart + period * CGFloat(n) - offset
            guard left < strip.maxX + 2, left + eventWidth > strip.minX - 2 else { continue }
            let rect = CGRect(x: left, y: strip.minY + 8, width: eventWidth, height: 30)
            let under = left <= nowX && left + eventWidth >= nowX
            let past = left + eventWidth < nowX
            let shape = Path(roundedRect: rect, cornerRadius: 5, style: .continuous)
            let ink = past ? 0.35 : 1.0
            band.fill(shape, with: .color(VignettePalette.card))
            band.fill(shape, with: .color(VignettePalette.avatar[concert].opacity(past ? 0.1 : under ? 0.34 : 0.24)))
            band.stroke(shape, with: .color(under ? VignettePalette.phosphor.opacity(0.8) : VignettePalette.edge), lineWidth: under ? 0.7 : 0.5)
            var art = band
            art.opacity *= ink
            CalendarScene.photo(&art, CGRect(x: rect.minX + 4, y: rect.minY + 4, width: 22, height: 22), corner: 3.5)
            let x = rect.minX + 30
            VignetteKit.bar(&band, CGRect(x: x, y: rect.minY + 6, width: 30, height: 3), VignettePalette.ink.opacity(0.85 * ink))
            VignetteKit.bar(&band, CGRect(x: x, y: rect.minY + 13.4, width: 12, height: 2.2), VignettePalette.ink.opacity(0.55 * ink))
            VignetteKit.pinMark(&band, tip: CGPoint(x: x + 1.6, y: rect.minY + 23.4), size: 4, color: VignettePalette.ink.opacity(0.5 * ink))
            VignetteKit.bar(&band, CGRect(x: x + 5, y: rect.minY + 20.2, width: 24, height: 2.2), VignettePalette.ink.opacity(0.55 * ink))

            // Only the coming concert is the one being reminded about.
            guard n == 0 else { continue }
            let gone = ease(t, onFrom, onFrom + 0.3)
            for (index, lead) in leads.enumerated() {
                let x = left - lead
                let shown = ease(t, ticks[index], ticks[index] + 0.12)
                let chosen = index == picked ? ease(t, pick, pick + 0.14) : 0
                let dropped = index == picked ? 0 : ease(t, pick + 0.05, pick + 0.25)
                let alpha = shown * (1 - dropped) * (1 - gone)
                guard alpha > 0 else { continue }
                var dot = band
                dot.opacity *= alpha
                if chosen > 0 { dot.addFilter(.shadow(color: VignettePalette.phosphor.opacity(0.85 * chosen), radius: 2.5)) }
                dot.fill(Path(CGRect(x: x - 0.3, y: base - 7, width: 0.6, height: 7)), with: .color(VignettePalette.ink.opacity(VignetteClock.mix(0.35, 0, chosen))))
                dot.fill(Path(CGRect(x: x - 0.4, y: base - 7, width: 0.8, height: 7)), with: .color(VignettePalette.phosphor.opacity(chosen)))
                let r = VignetteClock.mix(1.5, 2.1, chosen)
                dot.fill(Path(ellipseIn: CGRect(x: x - r, y: base - r, width: 2 * r, height: 2 * r)), with: .color(VignettePalette.ink.opacity(0.55 * (1 - chosen))))
                dot.fill(Path(ellipseIn: CGRect(x: x - r, y: base - r, width: 2 * r, height: 2 * r)), with: .color(VignettePalette.phosphorHot.opacity(chosen)))
            }
            // The lead, lit from the pick to the doors.
            let span = ease(t, pick + 0.05, pick + 0.32) * (1 - gone)
            if span > 0 {
                let from = left - leads[picked]
                var glow = band
                glow.addFilter(.shadow(color: VignettePalette.phosphor.opacity(0.8), radius: 2))
                glow.fill(
                    Path(roundedRect: CGRect(x: from, y: base - 0.7, width: leads[picked] * CGFloat(span), height: 1.4), cornerRadius: 0.7),
                    with: .color(VignettePalette.phosphor))
            }
            // The model's mark, over the event while it chooses.
            let mark = ease(t, glint, glint + 0.2) * (1 - ease(t, pick + 0.2, pick + 0.45))
            if mark > 0 {
                var glow = band
                glow.opacity *= mark
                glow.addFilter(.shadow(color: VignettePalette.phosphor.opacity(0.8), radius: 2.5))
                VignetteKit.sparkles(&glow, center: CGPoint(x: left + eventWidth - 4, y: strip.minY + 4), size: 5.5, color: VignettePalette.phosphorHot)
            }
        }

        // Now: a fixed line, and the day goes by under it.
        var glow = ctx
        glow.addFilter(.shadow(color: VignettePalette.phosphor.opacity(0.8), radius: 2.5))
        glow.fill(Path(roundedRect: CGRect(x: nowX - 0.5, y: strip.minY, width: 1, height: strip.height), cornerRadius: 0.5),
                  with: .color(VignettePalette.phosphor))
        glow.fill(Path(ellipseIn: CGRect(x: nowX - 2, y: strip.minY - 2, width: 4, height: 4)), with: .color(VignettePalette.phosphorHot))
    }

    // MARK: the banner

    /// The notification: the app's mark, the model's line, the time and the
    /// place, and Tickets.
    private static func drawBanner(_ ctx: inout GraphicsContext, t: Double) {
        let ease = VignetteClock.ease
        let arrive = ease(t, bannerIn, bannerIn + 0.3)
        let leave = ease(t, open, open + 0.25)
        guard arrive > 0, leave < 1 else { return }
        var layer = ctx
        layer.opacity *= arrive * (1 - leave)
        layer.translateBy(x: 0, y: -8 * CGFloat(1 - arrive))
        let shape = Path(roundedRect: banner, cornerRadius: 8, style: .continuous)
        var lifted = layer
        lifted.addFilter(.shadow(color: Color.black.opacity(0.5), radius: 6))
        lifted.fill(shape, with: .color(VignettePalette.cap))
        layer.stroke(shape, with: .color(Color.white.opacity(0.14)), lineWidth: 0.5)

        let icon = CGRect(x: banner.minX + 6, y: banner.minY + 6, width: 12, height: 12)
        layer.fill(
            Path(roundedRect: icon, cornerRadius: 3, style: .continuous),
            with: .linearGradient(
                Gradient(colors: [VignettePalette.phosphorHot, VignettePalette.phosphor]),
                startPoint: CGPoint(x: icon.midX, y: icon.minY), endPoint: CGPoint(x: icon.midX, y: icon.maxY)))
        VignetteKit.calendarMark(&layer, center: CGPoint(x: icon.midX, y: icon.midY + 0.4), size: 6.4, color: .white)

        // The line the model wrote, writing itself on, actor colour cooling
        // to ink.
        let x = banner.minX + 22
        let written = ease(t, bannerIn + 0.12, bannerIn + 0.5)
        let cooled = ease(t, bannerIn + 0.45, bannerIn + 0.75)
        let title = CGRect(x: x, y: banner.minY + 6.5, width: max(3, 60 * CGFloat(written)), height: 3)
        VignetteKit.bar(&layer, title, VignettePalette.phosphorHot.opacity(1 - cooled))
        VignetteKit.bar(&layer, title, VignettePalette.ink.opacity(0.92 * cooled))
        VignetteKit.bar(&layer, CGRect(x: banner.maxX - 15, y: banner.minY + 6.8, width: 9, height: 2.2), VignettePalette.dim)
        // "8pm · The Fillmore".
        VignetteKit.bar(&layer, CGRect(x: x, y: banner.minY + 13.2, width: 10, height: 2.4), VignettePalette.ink.opacity(0.6))
        layer.fill(Path(ellipseIn: CGRect(x: x + 12.4, y: banner.minY + 13.8, width: 1.3, height: 1.3)), with: .color(VignettePalette.dim))
        VignetteKit.bar(&layer, CGRect(x: x + 16, y: banner.minY + 13.2, width: 34, height: 2.4), VignettePalette.ink.opacity(0.6))

        // Tickets: the helper, the one thing to press.
        let pressed = VignetteClock.press(t, at: press)
        VignetteKit.pill(&layer, button, label: 13, labelX: button.minX + 14, pressed: pressed)
        VignetteKit.ticketMark(
            &layer, center: CGPoint(x: button.minX + 7.5, y: button.midY + CGFloat(pressed)), size: 6,
            color: pressed > 0.5 ? .white : VignettePalette.phosphorHot)
    }

    // MARK: the tickets email

    private static func drawMail(_ ctx: inout GraphicsContext, t: Double, shown: Double) {
        guard shown > 0.001 else { return }
        let ease = VignetteClock.ease
        var layer = ctx
        layer.opacity *= shown
        let rise = ease(t, open, open + 0.3), lift = ease(t, close, close + 0.25)
        layer.translateBy(x: 0, y: 8 * CGFloat(1 - rise) - 8 * CGFloat(lift))
        let shape = Path(roundedRect: mail, cornerRadius: 8, style: .continuous)
        var lifted = layer
        lifted.addFilter(.shadow(color: Color.black.opacity(0.5), radius: 6))
        lifted.fill(shape, with: .color(VignettePalette.card))
        layer.fill(shape, with: .color(VignettePalette.faint))
        layer.stroke(shape, with: .color(Color.white.opacity(0.12)), lineWidth: 0.5)

        let x = mail.minX + 10
        VignetteKit.avatar(&layer, center: CGPoint(x: x + 5, y: mail.minY + 12), diameter: 10, tint: 2)
        VignetteKit.bar(&layer, CGRect(x: x + 14, y: mail.minY + 8.6, width: 30, height: 2.8), VignettePalette.ink.opacity(0.8))
        VignetteKit.bar(&layer, CGRect(x: x + 14, y: mail.minY + 14.2, width: 44, height: 2.2), VignettePalette.dim)
        VignetteKit.bar(&layer, CGRect(x: mail.maxX - 24, y: mail.minY + 9, width: 14, height: 2.2), VignettePalette.dim)
        VignetteKit.bar(&layer, CGRect(x: x, y: mail.minY + 25, width: 64, height: 3.6), VignettePalette.ink.opacity(0.9))

        // The ticket, lit as it arrives.
        let stub = CGRect(x: x, y: mail.minY + 36, width: mail.width - 20, height: 38)
        let ticket = VignetteKit.ticketPath(stub)
        let lit = ease(t, open + 0.15, open + 0.35) * (1 - ease(t, open + 0.6, close))
        var glow = layer
        if lit > 0 { glow.addFilter(.shadow(color: VignettePalette.phosphor.opacity(0.7 * lit), radius: 4)) }
        glow.fill(ticket, with: .color(VignettePalette.phosphor.opacity(0.13)))
        layer.stroke(ticket, with: .color(VignettePalette.phosphor.opacity(0.55 + 0.35 * lit)), lineWidth: 0.7)
        let tear = stub.minX + stub.width * 0.68
        var perforation = Path()
        perforation.move(to: CGPoint(x: tear, y: stub.minY + 4))
        perforation.addLine(to: CGPoint(x: tear, y: stub.maxY - 4))
        layer.stroke(perforation, with: .color(VignettePalette.phosphor.opacity(0.6)), style: StrokeStyle(lineWidth: 0.6, dash: [1.4, 1.6]))
        let sx = stub.minX + 9
        VignetteKit.bar(&layer, CGRect(x: sx, y: stub.minY + 8, width: 40, height: 3.4), VignettePalette.ink.opacity(0.9))
        VignetteKit.bar(&layer, CGRect(x: sx, y: stub.minY + 17, width: 22, height: 2.4), VignettePalette.ink.opacity(0.6))
        VignetteKit.bar(&layer, CGRect(x: sx, y: stub.minY + 24, width: 34, height: 2.4), VignettePalette.ink.opacity(0.6))
        // A barcode on the stub, end on.
        var bx = tear + 6
        let widths: [CGFloat] = [1.2, 0.6, 0.6, 1.6, 0.6, 1.2, 0.6, 0.9, 1.6, 0.6, 1.2]
        for w in widths where bx + w < stub.maxX - 6 {
            layer.fill(Path(CGRect(x: bx, y: stub.minY + 8, width: w, height: stub.height - 16)), with: .color(VignettePalette.ink.opacity(0.75)))
            bx += w + 0.9
        }
        VignetteKit.bar(&layer, CGRect(x: x, y: mail.maxY - 16, width: 78, height: 2.2), VignettePalette.dim)
        VignetteKit.bar(&layer, CGRect(x: x, y: mail.maxY - 10.5, width: 52, height: 2.2), VignettePalette.dim)
    }
}

// MARK: - sign in with Outlook

/// SIGN IN WITH OUTLOOK: the add-account sheet over the window, and two
/// doors on it, side by side: Gmail, its envelope solid, and Outlook, its
/// envelope in outline, each with its name, as the app's own buttons are
/// drawn. A pointer goes to Outlook and presses; the door works for a breath
/// and takes its check. The sheet goes, and in the window's account switcher
/// the Outlook account joins the Gmail one, wearing the same envelope it was
/// signed in with, and becomes the one you are looking at: its inbox comes
/// up under it.
///
/// The seam: the sheet comes back for the next account, and the window
/// behind it is the one the loop began on.
enum OutlookSignInScene: VignetteDrawing {
    /// Outlook chosen and checked, beside Gmail: the door is the news.
    static let posterTime: Double = 1.62

    private static let window = CGRect(x: 10, y: 10, width: 220, height: 100)
    private static let sheet = CGRect(x: 40, y: 12, width: 160, height: 94)
    private static let gmail = CGRect(x: 50, y: 50, width: 66, height: 36)
    private static let outlook = CGRect(x: 124, y: 50, width: 66, height: 36)
    private static let switcherY: CGFloat = 19.5

    // The beats.
    private static let press: Double = 0.95
    private static let checked: Double = 1.45
    private static let sheetOut: Double = 1.75
    private static let join: Double = 2.05
    private static let inbox: Double = 2.4
    private static let sheetBack: Double = 3.95
    /// Behind the returning sheet, once it covers the switcher.
    private static let reset: Double = 4.3

    static func draw(_ ctx: inout GraphicsContext, t: Double) {
        let ease = VignetteClock.ease
        let sheetShown = 1 - ease(t, sheetOut, sheetOut + 0.3) + ease(t, sheetBack, sheetBack + 0.32)
        let joined = t < reset ? ease(t, join, join + 0.4) : 0
        let switched = t < reset ? ease(t, inbox, inbox + 0.5) : 0
        drawWindow(&ctx, joined: joined, switched: switched, t: t)

        guard sheetShown > 0.001 else { return }
        ctx.fill(Path(roundedRect: window, cornerRadius: 8, style: .continuous), with: .color(Color.black.opacity(0.45 * sheetShown)))
        var layer = ctx
        layer.opacity *= sheetShown
        let leaving = t < sheetBack
        layer.translateBy(x: 0, y: (leaving ? 8 : 6) * CGFloat(1 - sheetShown))
        drawSheet(&layer, t: t, fresh: !leaving || t < press - 0.6)
    }

    // MARK: the window

    private static func drawWindow(_ ctx: inout GraphicsContext, joined: Double, switched: Double, t: Double) {
        let ease = VignetteClock.ease
        let shape = Path(roundedRect: window, cornerRadius: 8, style: .continuous)
        ctx.fill(shape, with: .color(VignettePalette.card.opacity(0.7)))
        ctx.stroke(shape, with: .color(VignettePalette.edge), lineWidth: 0.5)
        for index in 0..<3 {
            ctx.fill(
                Path(ellipseIn: CGRect(x: window.minX + 7 + CGFloat(index) * 5.5, y: window.minY + 7.5, width: 3, height: 3)),
                with: .color(Color.white.opacity(0.16)))
        }
        ctx.fill(Path(CGRect(x: window.minX, y: window.minY + 19, width: window.width, height: 0.5)), with: .color(VignettePalette.edge))

        // The switcher: Gmail alone, then Outlook beside it, then Outlook on.
        let capsuleW: CGFloat = VignetteClock.mix(18, 34, joined)
        let capsule = CGRect(x: window.midX - capsuleW / 2, y: switcherY - 7.5, width: capsuleW, height: 15)
        ctx.fill(Path(roundedRect: capsule, cornerRadius: 7.5), with: .color(VignettePalette.faint))
        ctx.stroke(Path(roundedRect: capsule, cornerRadius: 7.5), with: .color(VignettePalette.edge), lineWidth: 0.5)
        let gx = VignetteClock.mix(window.midX, window.midX - 8, joined)
        account(&ctx, center: CGPoint(x: gx, y: switcherY), tint: 0, filled: true, on: 1 - switched, opacity: 1)
        if joined > 0 {
            let ox = VignetteClock.mix(window.midX + 14, window.midX + 8, joined)
            var glow = ctx
            let flash = ease(t, join + 0.2, join + 0.4) * (1 - ease(t, join + 0.6, join + 1.2))
            if flash > 0 { glow.addFilter(.shadow(color: VignettePalette.phosphor.opacity(0.9 * flash), radius: 4)) }
            account(&glow, center: CGPoint(x: ox, y: switcherY), tint: 1, filled: false, on: switched, opacity: joined)
        }

        // The sidebar and the inbox: Gmail's mail, then Outlook's, row by row.
        let rail = CGRect(x: window.minX, y: window.minY + 19.5, width: 38, height: window.height - 19.5)
        ctx.fill(Path(CGRect(x: rail.maxX - 0.25, y: rail.minY, width: 0.5, height: rail.height)), with: .color(VignettePalette.edge))
        for (index, w) in [CGFloat(20), 16, 22, 14].enumerated() {
            let y = rail.minY + 9 + CGFloat(index) * 9
            if index == 0 {
                ctx.fill(Path(roundedRect: CGRect(x: rail.minX + 4, y: y - 3.5, width: rail.width - 8, height: 7), cornerRadius: 3.5), with: .color(Color.white.opacity(0.07)))
            }
            VignetteKit.bar(&ctx, CGRect(x: rail.minX + 8, y: y - 1.1, width: w, height: 2.2), index == 0 ? VignettePalette.ink.opacity(0.7) : VignettePalette.dim)
        }
        let rows: [[(tint: Int, sender: CGFloat, subject: CGFloat)]] = [
            [(0, 26, 54), (2, 20, 66), (0, 30, 46), (2, 22, 60), (0, 24, 50), (2, 28, 58)],
            [(1, 22, 60), (2, 28, 48), (1, 24, 64), (1, 30, 52), (2, 20, 58), (1, 26, 46)],
        ]
        var list = ctx
        VignetteKit.clipFading(&list, CGRect(x: rail.maxX, y: rail.minY, width: window.maxX - rail.maxX, height: rail.height - 2), fade: 14)
        for index in 0..<6 {
            let p = ease(switched, Double(index) * 0.08, 0.5 + Double(index) * 0.08)
            let y = rail.minY + 6 + CGFloat(index) * 13
            for (which, set) in rows.enumerated() {
                let alpha = which == 0 ? 1 - p : p
                guard alpha > 0.001 else { continue }
                var row = list
                row.opacity *= alpha
                row.translateBy(x: 0, y: which == 0 ? 0 : 4 * CGFloat(1 - p))
                let r = set[index]
                let mid = y + 4
                VignetteKit.avatar(&row, center: CGPoint(x: rail.maxX + 10, y: mid), diameter: 6, tint: r.tint)
                VignetteKit.bar(&row, CGRect(x: rail.maxX + 18, y: mid - 1.4, width: r.sender, height: 2.8), VignettePalette.ink.opacity(0.7))
                VignetteKit.bar(&row, CGRect(x: rail.maxX + 54, y: mid - 1.4, width: r.subject, height: 2.8), VignettePalette.ink.opacity(0.55))
                VignetteKit.bar(
                    &row, CGRect(x: rail.maxX + 58 + r.subject, y: mid - 1.2, width: 110 - r.subject, height: 2.4),
                    VignettePalette.dim.opacity(0.7))
            }
        }
    }

    /// One account in the switcher: its colour, its envelope, and an even
    /// ring when it is the one on.
    private static func account(
        _ ctx: inout GraphicsContext, center: CGPoint, tint: Int, filled: Bool, on: Double, opacity: Double
    ) {
        var layer = ctx
        layer.opacity *= opacity
        let d: CGFloat = 11.5
        layer.fill(
            Path(ellipseIn: CGRect(x: center.x - d / 2, y: center.y - d / 2, width: d, height: d)),
            with: .color(VignettePalette.avatar[tint].opacity(0.85)))
        VignetteKit.envelopeMark(&layer, center: center, size: 6, color: .white, filled: filled)
        if on > 0 {
            layer.stroke(
                Path(ellipseIn: CGRect(x: center.x - d / 2 - 1.6, y: center.y - d / 2 - 1.6, width: d + 3.2, height: d + 3.2)),
                with: .color(VignettePalette.ink.opacity(0.8 * on)), lineWidth: 0.7)
        }
    }

    // MARK: the sheet

    /// The sheet: a headline, a line, the two doors, and self-hosting as the
    /// quieter way in under them. `fresh` draws it as it opens, untouched.
    private static func drawSheet(_ ctx: inout GraphicsContext, t: Double, fresh: Bool) {
        let ease = VignetteClock.ease
        let shape = Path(roundedRect: sheet, cornerRadius: 10, style: .continuous)
        var lifted = ctx
        lifted.addFilter(.shadow(color: Color.black.opacity(0.55), radius: 8))
        lifted.fill(shape, with: .color(VignettePalette.cap))
        ctx.stroke(shape, with: .color(Color.white.opacity(0.14)), lineWidth: 0.5)

        VignetteKit.bar(&ctx, CGRect(x: sheet.midX - 34, y: sheet.minY + 14, width: 68, height: 4.6), VignettePalette.ink.opacity(0.9))
        VignetteKit.bar(&ctx, CGRect(x: sheet.midX - 48, y: sheet.minY + 25, width: 96, height: 2.4), VignettePalette.dim)
        VignetteKit.bar(&ctx, CGRect(x: sheet.midX - 30, y: sheet.minY + 30.5, width: 60, height: 2.4), VignettePalette.dim)

        let pressed = fresh ? 0 : VignetteClock.press(t, at: press)
        let chosen = fresh ? 0 : ease(t, press, press + 0.12)
        let working = fresh ? 0 : ease(t, press + 0.1, press + 0.2) * (1 - ease(t, checked - 0.08, checked))
        let check = fresh ? 0 : ease(t, checked, checked + 0.16)
        door(&ctx, gmail, name: "Gmail", filled: true, chosen: 0, pressed: 0, working: 0, check: 0, t: t)
        door(&ctx, outlook, name: "Outlook", filled: false, chosen: chosen, pressed: pressed, working: working, check: check, t: t)

        let quiet = CGRect(x: sheet.midX - 36, y: sheet.maxY - 16, width: 72, height: 9)
        ctx.stroke(Path(roundedRect: quiet, cornerRadius: 4.5), with: .color(VignettePalette.edge), lineWidth: 0.5)
        VignetteKit.bar(&ctx, CGRect(x: quiet.minX + 14, y: quiet.midY - 1.1, width: 44, height: 2.2), VignettePalette.dim)

        // The pointer: from where it rests to Outlook, one press, then away.
        guard !fresh else { return }
        let from = CGPoint(x: 182, y: 104), to = CGPoint(x: outlook.maxX - 16, y: outlook.minY + 12)
        let move = ease(t, press - 0.55, press - 0.06)
        let alpha = ease(t, press - 0.7, press - 0.5) * (1 - ease(t, press + 0.2, press + 0.36))
        VignetteKit.pointer(
            &ctx, tip: CGPoint(x: VignetteClock.mix(from.x, to.x, move), y: VignetteClock.mix(from.y, to.y, move)),
            down: pressed, opacity: alpha)
    }

    /// One door: an envelope and its name. Chosen, it lights all the way
    /// round, works, and takes its check.
    private static func door(
        _ ctx: inout GraphicsContext, _ rect: CGRect, name: String, filled: Bool, chosen: Double, pressed: Double,
        working: Double, check: Double, t: Double
    ) {
        let rect = rect.offsetBy(dx: 0, dy: CGFloat(pressed))
        let shape = Path(roundedRect: rect, cornerRadius: 7, style: .continuous)
        var glow = ctx
        if pressed > 0 { glow.addFilter(.shadow(color: VignettePalette.phosphor.opacity(0.75 * pressed), radius: 5)) }
        glow.fill(shape, with: .color(Color.white.opacity(0.05)))
        glow.fill(shape, with: .color(VignettePalette.phosphor.opacity(0.16 * chosen + 0.5 * pressed)))
        ctx.stroke(shape, with: .color(VignettePalette.edge.opacity(1 - chosen)), lineWidth: 0.5)
        ctx.stroke(shape, with: .color(VignettePalette.phosphor.opacity(0.85 * chosen)), lineWidth: 0.7)

        let mark = CGPoint(x: rect.midX, y: rect.minY + 13)
        var envelope = ctx
        envelope.opacity *= (1 - working) * (1 - check)
        VignetteKit.envelopeMark(&envelope, center: mark, size: 11, color: VignettePalette.ink.opacity(0.85), filled: filled)
        VignetteKit.arc(
            &ctx, center: mark, radius: 3.6, head: t * 2.4, sweep: 0.3, color: VignettePalette.phosphorHot,
            lineWidth: 0.9, opacity: working)
        VignetteKit.check(&ctx, center: mark, size: 3.6, progress: check)
        let label = Text(name)
            .font(.system(size: 6.5, weight: .semibold))
            .foregroundStyle(VignettePalette.ink.opacity(0.85))
        ctx.draw(label, at: CGPoint(x: rect.midX, y: rect.minY + 27), anchor: .center)
    }
}
