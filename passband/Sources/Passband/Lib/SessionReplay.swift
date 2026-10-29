// Session replay for the Mac app, as layout only: PostHog's recordings player
// shows the layout of the window moving, and where the clicks landed, and
// nothing that was on the screen. See `ReplayWireframe` for what a frame is
// made of and why no text, image or mail body can be in one.
//
// PostHog has no macOS replay, so this speaks its WEB replay format by hand:
// `$snapshot` events to /s/ carrying rrweb records (4 = meta, the canvas size
// and the screen as a URL; 2 = a full snapshot, a small HTML page of boxes;
// 3 = pointer moves and clicks on its <body>), under the same `$session_id` as
// the ordinary events so they all land in one recording. Not the mobile
// format: PostHog silently drops mobile-format recordings from a custom client.
// `Analytics.replay` is the one door out, and it validates every
// record against the closed vocabulary before it is queued.
//
// Cost: a frame is a walk of the window's layer tree on the main thread (a few
// hundred layers on the sitrep), at most twice a second plus once per click,
// and only while someone is using the app. An unchanged screen is never
// re-sent, nothing is recorded while the app is in the background, and two
// idle minutes stop the recording until the next click or key. Replay runs only at the `full` telemetry level.
//
// Activity: PostHog's recordings list hides any recording with 5 or fewer
// "active seconds", and it counts those from pointer records, not from
// frames. So while someone moves the pointer, scrolls or types, the pointer's
// position goes out as an rrweb mouse-move at most once a tick. A key press
// counts as activity at the pointer's position and never says which key.
//
// Frustration comes out of the click stream as two events PostHog's
// recordings list can filter on: `$rageclick` (three clicks in one spot inside
// a second) and `$dead_click` (a click after which nothing on screen changed
// for 1.5 s). Both are heuristics and both say only WHERE, never what. Clicks
// in a web view or a text field count as neither (a triple-click there selects
// a paragraph; a link may have opened the browser), and a click that opens a
// menu is not dead, since menus are not in the picture.

#if os(macOS)
    import AppKit

    @MainActor
    final class SessionReplay {
        static let shared = SessionReplay()

        private static let tickEvery: TimeInterval = 0.5
        /// At most one frame a second.
        private static let throttle: TimeInterval = 1.0
        /// No input for this long and the recording pauses.
        private static let idleAfter: TimeInterval = 120
        private static let deadAfter: TimeInterval = 1.5
        /// A pending click older than this is forgotten, not reported: the
        /// tick that should have judged it did not run (app hidden, window
        /// minimized), so nothing is known about what it did.
        private static let deadForget: TimeInterval = 3
        private static let rageWindow: TimeInterval = 1.0
        private static let rageRadius: CGFloat = 30

        private var timer: Timer?
        private var lastInput = Date.distantPast
        private var lastSent = Date.distantPast
        private var lastPrint: Int?
        private var lastMeta: Meta?
        private var clicks: [(at: Date, point: CGPoint)] = []
        private var pendingDead: [(at: Date, print: Int, point: CGPoint, origin: CGPoint)] = []
        private var menuTracking = false
        /// Input since the last activity record, and where the pointer was.
        private var activeSinceTick = false
        private var lastPointer: CGPoint?

        private struct Meta: Equatable {
            var width: Int
            var height: Int
            var href: String
        }

        private struct Frame {
            var nodes: [ReplayWireframe.Node]
            var width: Int
            var height: Int
            var background: String
            var print: Int
            /// Web views and text fields. A click there may be a link that
            /// opened the browser or a triple-click selecting a paragraph,
            /// which this picture cannot tell from frustration.
            var opaqueRects: [CGRect]
        }

        func start() {
            guard timer == nil else { return }
            ReplayIcons.shared.prepare()
            NSEvent.addLocalMonitorForEvents(
                matching: [.leftMouseDown, .leftMouseUp, .keyDown, .scrollWheel]
            ) { event in
                MainActor.assumeIsolated { SessionReplay.shared.observe(event) }
                return event
            }
            let t = Timer(timeInterval: Self.tickEvery, repeats: true) { _ in
                MainActor.assumeIsolated { _ = SessionReplay.shared.tick() }
            }
            // `.common` keeps frames coming while a scroll or drag is tracking.
            RunLoop.main.add(t, forMode: .common)
            timer = t
            // A click that opens a menu (a popup button, a context menu) changes
            // nothing in the picture, because menus are not windows we draw.
            let nc = NotificationCenter.default
            nc.addObserver(forName: NSMenu.didBeginTrackingNotification, object: nil, queue: .main) {
                _ in
                MainActor.assumeIsolated {
                    SessionReplay.shared.menuTracking = true
                    SessionReplay.shared.pendingDead.removeAll()
                }
            }
            nc.addObserver(forName: NSMenu.didEndTrackingNotification, object: nil, queue: .main) {
                _ in
                MainActor.assumeIsolated { SessionReplay.shared.menuTracking = false }
            }
            // Coming back to the app opens with a full picture: whatever
            // changed while it was in the background was not recorded.
            NotificationCenter.default.addObserver(
                forName: NSApplication.didBecomeActiveNotification, object: nil, queue: .main
            ) { _ in
                MainActor.assumeIsolated {
                    SessionReplay.shared.lastPrint = nil
                    SessionReplay.shared.lastMeta = nil
                    SessionReplay.shared.lastInput = Date()
                }
            }
        }

        // MARK: - input

        private func observe(_ event: NSEvent) {
            let now = Date()
            lastInput = now
            activeSinceTick = true
            guard Analytics.replayEnabled else { return }
            switch event.type {
            case .leftMouseDown, .leftMouseUp:
                guard let window = event.window,
                    let point = canvasPoint(screen: window.convertPoint(toScreen: event.locationInWindow))
                else { return }
                // The first click after a pause opens the recording with a
                // picture, or there would be nothing to draw the click on.
                var frame: Frame?
                if Analytics.replayNeedsFull || lastPrint == nil { frame = tick(force: true) }
                let down = event.type == .leftMouseDown
                // rrweb mouse interactions on <body>: 1 = down, 0 = up, and a
                // 2 = click after the up, which is what the player marks.
                let x = Int(point.x.rounded())
                let y = Int(point.y.rounded())
                func interaction(_ kind: Int) -> [String: Any] {
                    [
                        "type": 3, "timestamp": Self.millis(now),
                        "data": [
                            "source": 2, "type": kind, "id": ReplayWireframe.bodyId, "x": x, "y": y,
                        ],
                    ]
                }
                Analytics.replay(down ? [interaction(1)] : [interaction(0), interaction(2)], full: false)
                if down { noteClick(point, now, frame) }
            default:
                break
            }
        }

        private func noteClick(_ point: CGPoint, _ now: Date, _ known: Frame?) {
            // The picture as it was before the app saw this click (a local
            // monitor runs before dispatch), to compare against after.
            guard !menuTracking, let main = MainWindow.find(), let frame = known ?? capture(),
                !frame.opaqueRects.contains(where: { $0.contains(point) })
            else { return }
            clicks.removeAll { now.timeIntervalSince($0.at) > Self.rageWindow }
            clicks.append((now, point))
            let near = clicks.filter { hypot($0.point.x - point.x, $0.point.y - point.y) <= Self.rageRadius }
            if near.count >= 3 {
                Analytics.capture("$rageclick", frustration(point))
                clicks.removeAll()
            }
            pendingDead.append((now, frame.print, point, main.frame.origin))
        }

        private func frustration(_ point: CGPoint) -> [String: Any] {
            var props: [String: Any] = ["x": Int(point.x), "y": Int(point.y)]
            let screen = Analytics.currentScreen.get()
            if !screen.isEmpty { props["$screen_name"] = screen }
            return props
        }

        /// A screen point on the canvas: points from the main window's
        /// top-left, or nil outside it. Clicks and the pointer both use this.
        private func canvasPoint(screen: CGPoint) -> CGPoint? {
            guard let main = MainWindow.find() else { return nil }
            let p = CGPoint(x: screen.x - main.frame.minX, y: main.frame.maxY - screen.y)
            guard p.x >= 0, p.y >= 0, p.x <= main.frame.width, p.y <= main.frame.height
            else { return nil }
            return p
        }

        // MARK: - frames

        /// One tick: maybe a frame. Returns the frame it captured (sent or
        /// not), so a click can reuse it instead of walking the layers again.
        @discardableResult
        private func tick(force: Bool = false) -> Frame? {
            guard Analytics.replayEnabled, NSApp.isActive else {
                pendingDead.removeAll()
                // Telemetry going below `full` and back, or the app coming
                // back, must reopen with a full picture.
                lastPrint = nil
                return nil
            }
            let now = Date()
            notePointer(now)
            guard force || now.timeIntervalSince(lastInput) < Self.idleAfter else { return nil }
            guard let frame = capture() else {
                // Minimized or hidden: whatever a pending click did is unknowable.
                pendingDead.removeAll()
                return nil
            }
            resolveDeadClicks(frame.print, now)

            let needsFull = force || Analytics.replayNeedsFull
            let href = Analytics.currentScreen.get()
            let meta = Meta(width: frame.width, height: frame.height, href: href)
            let changed = frame.print != lastPrint || meta != lastMeta
            guard needsFull || changed else { return frame }
            guard needsFull || now.timeIntervalSince(lastSent) >= Self.throttle else { return frame }

            // rrweb wants a meta record before every full snapshot.
            var records: [[String: Any]] = [
                ReplayWireframe.meta(
                    screen: href, width: frame.width, height: frame.height,
                    timestamp: Self.millis(now))
            ]
            records.append(
                ReplayWireframe.fullSnapshot(
                    frame.nodes, width: frame.width, height: frame.height,
                    background: frame.background, timestamp: Self.millis(now)))
            Analytics.replay(records, full: true)
            lastSent = now
            lastPrint = frame.print
            lastMeta = meta
            return frame
        }

        /// The pointer, sampled each tick (mouse-moved events only reach
        /// windows that ask for them, so a monitor would miss most). A move
        /// counts as input; any input since the last tick goes out as one
        /// mouse-move at the pointer's canvas position, once the recording
        /// has a frame to draw it on.
        private func notePointer(_ now: Date) {
            if let p = canvasPoint(screen: NSEvent.mouseLocation) {
                if let last = lastPointer, hypot(last.x - p.x, last.y - p.y) >= 2 {
                    lastInput = now
                    activeSinceTick = true
                }
                lastPointer = p
            }
            guard activeSinceTick, let at = lastPointer, lastPrint != nil else { return }
            activeSinceTick = false
            Analytics.replay(
                [[
                    "type": 3, "timestamp": Self.millis(now),
                    "data": [
                        "source": 1,
                        "positions": [[
                            "x": Int(at.x.rounded()), "y": Int(at.y.rounded()),
                            "id": ReplayWireframe.bodyId,
                            "timeOffset": 0,
                        ]],
                    ],
                ]], full: false)
        }

        private func resolveDeadClicks(_ print: Int, _ now: Date) {
            let origin = MainWindow.find()?.frame.origin
            pendingDead.removeAll { click in
                if print != click.print { return true }  // something answered
                // The click moved the window (a title-bar drag): it did something.
                if origin != click.origin { return true }
                if now.timeIntervalSince(click.at) > Self.deadForget { return true }
                guard now.timeIntervalSince(click.at) >= Self.deadAfter else { return false }
                Analytics.capture("$dead_click", frustration(click.point))
                return true
            }
        }

        /// The app's visible windows, back to front, drawn onto the main
        /// window's canvas: a sheet, a popover or the login-code panel shows
        /// where it sits over the window. Anything outside the main window's
        /// frame is cut off, so a panel on another screen draws nothing.
        private func capture() -> Frame? {
            guard let main = MainWindow.find(), main.isVisible, !main.isMiniaturized,
                main.occlusionState.contains(.visible)
            else { return nil }
            let canvas = main.frame
            let bounds = CGRect(origin: .zero, size: canvas.size)
            ReplayIcons.shared.beginFrame()
            var nodes: [ReplayWireframe.Node] = []
            var opaqueRects: [CGRect] = []

            // Tooltips and menus are left out: a tooltip can be a sender's
            // address (`.help(sender)`), drawn as a bar its length.
            let windows = NSApp.windows
                .filter { $0.isVisible && !$0.isMiniaturized && $0.alphaValue > 0.01 }
                .filter {
                    let name = NSStringFromClass(type(of: $0))
                    return !name.contains("ToolTip") && !name.contains("Menu")
                }
                .filter { $0.frame.intersects(canvas) }
                .sorted { $0.orderedIndex > $1.orderedIndex }
            for window in windows {
                guard let view = window.contentView, let layer = view.layer else { continue }
                let content = window.convertToScreen(view.convert(view.bounds, to: nil))
                let offset = CGPoint(x: content.minX - canvas.minX, y: canvas.maxY - content.maxY)
                let clip = CGRect(origin: offset, size: content.size).intersection(bounds)
                guard !clip.isNull else { continue }
                // A sheet or panel's own background: theme, like the main one.
                if window !== main, let fill = themePaint(window.backgroundColor, in: window) {
                    nodes.append(
                        .init(
                            x: Int(clip.minX), y: Int(clip.minY), w: Int(clip.width),
                            h: Int(clip.height), kind: .fill, fill: fill, radius: 10))
                }
                let start = nodes.count
                ReplayWireframe.nodes(
                    root: layer, flipped: view.isFlipped, offset: offset, clip: clip,
                    icon: { ReplayIcons.shared.icon(for: $0, size: $1) }, into: &nodes)
                for n in nodes[start...] where n.kind == .opaque {
                    opaqueRects.append(CGRect(x: n.x, y: n.y, width: n.w, height: n.h))
                }
            }
            let width = Int(canvas.width.rounded())
            let height = Int(canvas.height.rounded())
            return Frame(
                nodes: nodes, width: width, height: height,
                background: themePaint(main.backgroundColor, in: main) ?? "#1E1E1EFF",
                print: ReplayWireframe.fingerprint(nodes, width: width, height: height),
                opaqueRects: opaqueRects)
        }

        /// A WINDOW's background resolved in its appearance, so a dark window
        /// records dark. The only real paint in a frame; see
        /// `ReplayWireframe.backgroundPaint`.
        private func themePaint(_ color: NSColor?, in window: NSWindow) -> String? {
            guard let color else { return nil }
            var cg: CGColor?
            window.effectiveAppearance.performAsCurrentDrawingAppearance { cg = color.cgColor }
            return ReplayWireframe.backgroundPaint(cg)
        }

        private static func millis(_ d: Date) -> Int64 { Int64(d.timeIntervalSince1970 * 1000) }
    }
#endif
