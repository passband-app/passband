// MEASURING A THREAD BEFORE ANYBODY SCROLLS TO IT.
//
// A rendered email's height is not knowable without a layout engine. You can
// count its characters and be wrong by a factor of ten, because the tables and
// the pictures are where the height actually is. So the reader used to find out
// the only way it could: mount the message, let WebKit measure it, and take the
// correction as a shove. A first pass through a thread is one shove per
// message — and the minimap is drawn to a scale derived from the same bad
// numbers, so the window mark barely moves while you scroll.
//
// This measures the whole thread up front, off screen, through the SAME sandbox
// the reader renders it in: same stripped body, same CSP, same image policy,
// same column width, same collapsed quoted history. The heights go in
// FrameHeights, so by the time the reader reaches a message its size is a fact,
// and opening the thread again has nothing left to measure.
//
// ONE FRAME, LOADED SERIALLY. Twenty messages are twenty documents, not twenty
// web views: a web view is a content process (~76ms to stand up and a process
// to keep), while another document through a warm one is ~2ms. That is what
// makes this cheap enough to run every time a thread opens.
//
// AND THE HEIGHT IS ASKED FOR, NOT LISTENED FOR. The measuring script posts
// heights as it finds them, but a posted message says nothing about which
// document it came from: measured against a run of documents through one frame,
// the previous body's height lands in the next body's bucket about a quarter of
// the time, and no amount of waiting between loads reliably prevents it. So
// this waits for the navigation IT started to finish, then calls
// `__passbandHeight()` — a question whose answer can only be about the document
// that was asked.

import SwiftUI
import WebKit

@MainActor
final class FrameMeasurer {
    static let shared = FrameMeasurer()
    private init() {}

    /// THE MEASURING FRAME'S VIEWPORT, and the whole reason this pass exists in
    /// a file of its own: it is a CONSTANT. The reader's own frames are sized to
    /// what they measure, which makes their viewport a function of their last
    /// answer; measuring here instead breaks that circle, because nothing this
    /// frame reports ever changes the frame it was reported from.
    ///
    /// The width is the reading column's and is load-bearing — mail reflows, and
    /// the same body measured at 843 and at 500 differs by a third.
    ///
    /// The height matters for one thing only, but it matters a lot for that one:
    /// `height:100%`, `height="100%"`, floats overflowing the body box and
    /// absolutely positioned children all measure the same at viewport 100, 800
    /// or 2400. `vh` units do not — they are whatever this says. So it is the
    /// READER'S OWN WINDOW, which is what makes `100vh` in an email mean one
    /// screenful, the same thing it means in a browser.
    private var viewportHeight: CGFloat = 800
    /// How long a document has to hold the same height before it is believed.
    /// Short, because the navigation has already done the waiting: `didFinish`
    /// does not fire until the subresources are in, so an image delayed two
    /// seconds is still counted before the first poll. This only catches a
    /// document that is still settling after that.
    private static let quiet: Duration = .milliseconds(25)
    /// The longest the POLLING may go on for a document that will not hold
    /// still.
    private static let deadline: Duration = .milliseconds(1200)
    /// And the longest we wait for the load itself. `didFinish` waiting for
    /// every subresource is what makes the poll trustworthy, and it is also
    /// what makes it hostage: with remote images turned on, one body pointing
    /// at a host that never answers stalls on the fetch timeout — measured
    /// still in flight at ten seconds — and takes the whole pass with it. The
    /// mail is not worth that; a guess is fine for one message.
    private static let loadDeadline: Duration = .seconds(2)

    /// THE ONE FRAME, KEPT ACROSS PASSES. It used to be let go at the end of
    /// every pass and built again at the start of the next, which put a
    /// content-process launch (~70ms) at the front of every thread's measuring
    /// — the pass runs 120ms after the reader opens, so the newest message
    /// sat at a guessed height for the first two hundred milliseconds of
    /// every open. An idle frame costs a parked process and nothing else, and
    /// a document through a warm one is two milliseconds.
    private var frame: WKWebView?
    private var relay: MeasureRelay?
    private var pass: Task<Void, Never>?
    /// The width the documents are rendered at: the reader's BODY width for
    /// the style being measured. Not `FrameHeights.width` — that is the
    /// column, style-independent, the number the heights are filed under.
    /// Declaring the body width there would wipe the column's file every
    /// time a pass began.
    private var width: CGFloat = 0
    /// Whose pass this is — the thread id. See `cancel(token:)`.
    private var token: String?
    /// WHICH ARRANGEMENT is being measured. It decides the key each height is
    /// filed under, and the caller has already turned it into the `width` these
    /// documents are rendered at — a bubble is a narrower measure than a card,
    /// so the same message is two different heights and neither is the other's.
    private var style: ThreadStyle = .classic
    /// THE DENSITY the documents are drawn at, taken once per pass: a card's
    /// padding and its fallback type are part of its height, and a pass that
    /// read the setting per message would file one thread under two answers.
    private var metrics: ReaderMetrics = .compact

    // MARK: - the pass

    /// One document to measure: a message as one version of it draws, and the
    /// key that version's height is filed under.
    private struct Job {
        let shown: ClientMessage
        let key: String
    }

    /// Measure every message in this thread that has no height on file, and
    /// call back ONCE when the whole thread is measured. That includes calling
    /// back immediately with nothing to do, which is what a second open looks
    /// like.
    func measure(
        _ messages: [ClientMessage], width: CGFloat, viewport: CGFloat, style: ThreadStyle,
        allowRemote: Bool, token: String, onFinished: @escaping () -> Void
    ) {
        reset()
        guard width > 0 else {
            releaseIfPressed()
            return
        }
        // Before the filter below asks what is already on file: a height taken
        // at the other density is on file and wrong.
        ReaderMetrics.sync()
        let metrics = ReaderMetrics.current
        self.metrics = metrics
        self.width = width
        // A window too short to be a reading surface is a window mid-animation;
        // a screenful is the better guess than a sliver.
        self.viewportHeight = max(400, viewport)
        self.style = style
        self.token = token

        // NEWEST FIRST: the thread opens on the newest message with the history
        // above it, so the next size anybody needs is the one just behind them.
        //
        // BOTH VERSIONS of a translated message: the English first, since that
        // is what opens, and then, behind every message's English, the
        // original, so "show original" opens at its real height rather than a
        // guess that snaps. Each is the document the card draws for that
        // version, under that version's key.
        let newestFirst = Array(messages.reversed())
        let versions: [Job] =
            newestFirst.map { Job(shown: $0.presented(), key: style.frameKey(for: $0)) }
            + newestFirst.filter { $0.readyTranslation != nil }.map {
                Job(shown: $0.presented(original: true), key: style.frameKey(for: $0, original: true))
            }
        let jobs = versions.filter { job in
            guard let html = job.shown.html, !html.isEmpty else { return false }
            // AUTHORITATIVE, not merely present: a height a live frame put
            // there is a stopgap taken inside a box it had sized itself, and
            // replacing exactly those is what this pass is for.
            guard FrameHeights.shared.authoritative(job.key) == nil else { return false }
            // Measured on an earlier launch, for exactly this document at
            // this width and density: filed without a WebKit load.
            if let kept = FrameHeightStore.shared.height(
                job.key, signature: FrameHeightFile.signature(html: html, allowRemote: allowRemote))
            {
                FrameHeights.shared.set(job.key, kept, authoritative: true)
                return false
            }
            return true
        }
        guard !jobs.isEmpty else {
            releaseIfPressed()
            onFinished()
            return
        }

        pass = Task { @MainActor [weak self] in
            // A document that reports nothing is a slow email; two in a row is
            // a frame that is not measuring at all, and the right answer to
            // that is to stop rather than to spend a second and a bit per
            // message learning it twenty times.
            var misses = 0
            for job in jobs {
                if Task.isCancelled { return }
                guard let self else { return }
                let measured = await self.measure(job.shown, allowRemote: allowRemote)
                let height = measured.height
                if Task.isCancelled { return }
                // The setting moved while this document was being measured:
                // the number is true of a drawing nobody will see, and filed as
                // authoritative it would outrank the frame that is showing the
                // real one. The reader starts a pass of its own for the flip.
                guard ReaderMetrics.current == metrics else {
                    self.finish()
                    return
                }
                if height > 0 {
                    FrameHeights.shared.set(job.key, height, authoritative: true)
                    // Kept for the next launch only when it SETTLED: a number
                    // still moving at the deadline is fine for now and wrong
                    // to keep.
                    if measured.settled, let html = job.shown.html {
                        FrameHeightStore.shared.record(
                            job.key, height: height,
                            signature: FrameHeightFile.signature(html: html, allowRemote: allowRemote))
                    }
                    misses = 0
                } else {
                    misses += 1
                    if misses >= 2 { break }
                }
            }
            guard let self, !Task.isCancelled else { return }
            // The frame stays warm for the next thread (but see `finish`); only
            // the pass ends.
            self.finish()
            onFinished()
        }
    }

    #if os(iOS)
        /// Let the kept frame go while no pass is using it: the pool is
        /// draining (memory is short, or the app has left the screen) and an
        /// idle frame is a parked content process. The next pass builds
        /// another, a launch's ~76ms. One mid-pass is the open reader's and is
        /// left alone.
        func releaseIdleFrame() {
            guard pass == nil, let frame else { return }
            frame.stopLoading()
            frame.navigationDelegate = nil
            self.frame = nil
            relay = nil
        }
    #endif

    /// The reader leaving. TOKEN-SCOPED because a thread switch is two events
    /// in an order SwiftUI does not promise: the incoming reader may start its
    /// pass before the outgoing one says goodbye, and a bare cancel would then
    /// throw away the pass that had just begun.
    func cancel(token: String) {
        guard self.token == token else { return }
        finish()
    }

    /// A pass over, however it ended. On the phone, while the pool is holding
    /// back (`WebFramePool.isPressed`), the kept frame goes with it.
    private func finish() {
        reset()
        releaseIfPressed()
    }

    private func releaseIfPressed() {
        #if os(iOS)
            if WebFramePool.shared.isPressed { releaseIdleFrame() }
        #endif
    }

    /// Stop. Heights already taken stay taken — they are true regardless of
    /// what is on screen — and the frame stays too (see `frame`).
    private func reset() {
        pass?.cancel()
        pass = nil
        token = nil
        // Anything awaiting a navigation that will now never finish has to be
        // let go, or the task parks on a continuation forever. A claim that
        // load was holding at the policy gate goes with it: the frame is
        // told to stop, and a claim left standing would be spent on the next
        // document's own navigation attempt.
        relay?.abort()
        relay?.resetClaims()
        frame?.stopLoading()
    }

    // MARK: - one message

    /// Load this body and hand back what it measures, or 0 if it never says.
    /// One document's height, and whether it SETTLED (two equal answers in
    /// a row) rather than being the last of a run that was still moving.
    private func measure(_ message: ClientMessage, allowRemote: Bool) async -> (height: CGFloat, settled: Bool) {
        // Prepared the same way the reader prepares it (strip, dedupe, cid,
        // proxy) — a document measured is not a document rendered otherwise.
        // Off the main actor, and filed in the shared cache, so the card that
        // eventually mounts this message finds the work already done.
        let prepared = await Self.prepare(message)
        guard !prepared.html.isEmpty, !Task.isCancelled else { return (0, false) }

        let view = liveFrame()
        guard let relay else { return (0, false) }
        view.frame = CGRect(x: 0, y: 0, width: max(1, width), height: viewportHeight)
        // Claimed BEFORE the load, because the policy gate can be asked before
        // `loadHTMLString` has returned.
        relay.expectOwnLoad()
        guard
            let navigation = view.loadHTMLString(
                // THE SAME DOCUMENT THE READER LOADS, bubble chrome included: a
                // bubble is padded differently from a card, and a height
                // taken from the wrong one is filed as authoritative under
                // the key the right one reads.
                EmailFrameCoordinator.document(
                    html: prepared.html, allowRemote: allowRemote, bubble: style == .bubbles,
                    metrics: metrics),
                baseURL: nil)
        else {
            relay.forgetOwnLoad()
            return (0, false)
        }
        // THE NAVIGATION IS THE TOKEN. Until this one finishes, the frame is
        // still showing the previous message and anything it reports is that
        // message's answer.
        //
        // On a clock, because the load is the unbounded part (see
        // `loadDeadline`): the watchdog releases the wait, and the frame is
        // told to stop so a dead fetch is not still running under the next
        // message.
        let watchdog = Task { @MainActor [weak relay] in
            try? await Task.sleep(for: Self.loadDeadline)
            guard !Task.isCancelled else { return }
            relay?.abort()
        }
        let landed = await relay.settled(navigation)
        watchdog.cancel()
        guard landed else {
            view.stopLoading()
            return (0, false)
        }

        var previous: CGFloat = -1
        var silent = 0
        let started = ContinuousClock.now
        while ContinuousClock.now - started < Self.deadline {
            let height = await pull(view)
            if height > 0, height == previous { return (height, true) }
            // The script defines the function before the navigation finishes,
            // and an empty body is still 28 points of padding — so a document
            // answering zero is a document that did not run it. Three of those
            // is not a slow email, and waiting out the deadline for it only
            // delays the nineteen behind it.
            silent = height > 0 ? 0 : silent + 1
            if silent >= 3 { return (0, false) }
            previous = height
            try? await Task.sleep(for: Self.quiet)
            if Task.isCancelled { return (0, false) }
        }
        // Out of time with a number that was still moving: the last one it
        // said beats nothing, and beats a guess.
        return (max(0, previous), false)
    }

    /// What the document says it is right now. The injected script defines
    /// `__passbandHeight` on the collapsed document, so this is the size the
    /// reader will show — not the size with the quoted chain unrolled.
    private func pull(_ view: WKWebView) async -> CGFloat {
        await withCheckedContinuation { continuation in
            view.evaluateJavaScript("window.__passbandHeight ? window.__passbandHeight() : 0") {
                value, _ in
                let height = (value as? NSNumber).map { CGFloat($0.doubleValue) } ?? 0
                continuation.resume(returning: height)
            }
        }
    }

    /// `message` is already the version being measured (ClientMessage.presented),
    /// so its html is the document the card draws for it.
    private static func prepare(_ message: ClientMessage) async -> EmailWebView.Prepared {
        let html = message.html ?? ""
        let allow = message.allowsTrackers
        let parts = message.attachmentList
        let key = EmailWebView.Prepared.cacheKey(html, allow, parts)
        if let warm = PreparedBodies.shared.get(key) { return warm }
        let made = await Task.detached(priority: .utility) {
            EmailWebView.Prepared.make(from: html, allowTrackers: allow, attachments: parts)
        }.value
        PreparedBodies.shared.set(key, made)
        return made
    }

    /// The one frame, built on demand. NO WINDOW AND NO SUPERVIEW: a web view
    /// held in a variable loads, runs the injected script and lays out exactly
    /// as one in a window does (checked against three kinds of offscreen
    /// hosting — same height to the point, and no window can steal focus if
    /// there is no window).
    private func liveFrame() -> WKWebView {
        if let frame { return frame }
        let config = WKWebViewConfiguration()
        // The same layers the reader renders under, because a document measured
        // under a weaker policy is a different document: no page script, no
        // persistent store, images only through our own handlers, and every
        // navigation but ours refused.
        config.defaultWebpagePreferences.allowsContentJavaScript = false
        config.websiteDataStore = EmailFrame.sharedDataStore
        config.setURLSchemeHandler(ImageSchemeHandler.shared, forURLScheme: ImageProxy.scheme)
        config.setURLSchemeHandler(ImageSchemeHandler.shared, forURLScheme: CidProxy.scheme)

        // The measuring script rides along for what it does BEFORE it reports:
        // it collapses the quoted history, so the height is the one the reader
        // opens at. Its messages have nowhere to go — no handler is registered
        // under that name and `send` swallows the failure — which is the point.
        let controller = WKUserContentController()
        controller.addUserScript(
            WKUserScript(
                source: EmailFrame.measuringScript, injectionTime: .atDocumentEnd,
                forMainFrameOnly: true))
        config.userContentController = controller

        let relay = MeasureRelay()
        let view = WKWebView(
            frame: CGRect(
                x: 0, y: 0, width: max(1, width),
                height: viewportHeight),
            configuration: config)
        view.navigationDelegate = relay
        EmailFrame.pinLightAppearance(view)
        self.frame = view
        self.relay = relay
        return view
    }
}

/// The measuring frame's navigation delegate, and the reason a height can be
/// trusted: it says when the navigation WE started has finished, so the pull
/// that follows can only be about the document we loaded. It also keeps layer 4
/// intact — every navigation the frame did not ask for is refused.
@MainActor
private final class MeasureRelay: NSObject, WKNavigationDelegate {
    private var pendingOwnLoads = 0
    private var waiting: (navigation: WKNavigation, resume: (Bool) -> Void)?
    /// A navigation that landed before anybody asked about it. Calling an async
    /// method is a suspension point, so "load, then wait" can be two turns of
    /// the run loop and the answer can arrive in between.
    private var settledEarly: (navigation: WKNavigation, ok: Bool)?

    /// Wait for this navigation to finish. False means it failed, or that the
    /// pass was abandoned while it was in flight.
    func settled(_ navigation: WKNavigation) async -> Bool {
        if let early = settledEarly, early.navigation === navigation {
            settledEarly = nil
            return early.ok
        }
        return await withCheckedContinuation { continuation in
            waiting = (navigation, { continuation.resume(returning: $0) })
        }
    }

    /// Let go of a wait that will never be answered.
    func abort() {
        let pending = waiting
        waiting = nil
        settledEarly = nil
        pending?.resume(false)
    }

    private func land(_ navigation: WKNavigation?, ok: Bool) {
        guard let navigation else { return }
        if let pending = waiting, pending.navigation === navigation {
            waiting = nil
            pending.resume(ok)
            return
        }
        settledEarly = (navigation, ok)
    }

    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
        land(navigation, ok: true)
    }

    func webView(
        _ webView: WKWebView, didFail navigation: WKNavigation!, withError error: Error
    ) {
        land(navigation, ok: false)
    }

    func webView(
        _ webView: WKWebView, didFailProvisionalNavigation navigation: WKNavigation!,
        withError error: Error
    ) {
        land(navigation, ok: false)
    }

    /// LAYER 4, unchanged for being off screen: only the loads this frame
    /// started are allowed, and a document that tried to navigate itself
    /// (a meta refresh is the one way it could, with script off) is refused.
    func webView(
        _ webView: WKWebView, decidePolicyFor navigationAction: WKNavigationAction,
        decisionHandler: @escaping @MainActor @Sendable (WKNavigationActionPolicy) -> Void
    ) {
        if pendingOwnLoads > 0 {
            pendingOwnLoads -= 1
            decisionHandler(.allow)
            return
        }
        decisionHandler(.cancel)
    }

    func expectOwnLoad() { pendingOwnLoads += 1 }
    /// Every claim, dropped: the pass that made them is over and the frame has
    /// been told to stop.
    func resetClaims() { pendingOwnLoads = 0 }
    /// A load that was claimed and then never started. Left standing, the claim
    /// would be spent on somebody else's navigation.
    func forgetOwnLoad() { pendingOwnLoads = max(0, pendingOwnLoads - 1) }
}

extension ThreadStyle {
    /// The key for `message` as the reader draws it (ClientMessage.presented):
    /// translated unless there is no ready translation or the reader asked for
    /// the original. Here rather than beside `frameKey` because ThreadStyle (and
    /// ThreadPrefetch) are compiled by the tests without the wire types.
    func frameKey(for message: ClientMessage, original: Bool = false) -> String {
        frameKey(message.id, translated: !original && message.readyTranslation != nil)
    }
}

/// The active account's `FrameHeightFile`, held in memory and written a few
/// seconds after it changes. Read and written on the main actor alongside
/// `FrameHeights`; the file is small (a few hundred KB at its cap).
@MainActor
final class FrameHeightStore {
    static let shared = FrameHeightStore()
    private init() {}

    private var account: UUID?
    private var file: FrameHeightFile?
    private var save: Task<Void, Never>?
    private static let saveDelay: Duration = .seconds(3)
    private static let writer = DispatchQueue(label: "app.passband.frame-heights", qos: .utility)

    /// The question every height on file must have been asked under.
    private var conditions: (build: String, width: Double, metrics: String) {
        let m = ReaderMetrics.current
        return (
            Bundle.main.object(forInfoDictionaryKey: "CFBundleVersion") as? String ?? "",
            Double(FrameHeights.shared.width),
            "\(m.bodyFont)/\(m.cardPadding)/\(m.bubblePaddingV)/\(m.bubblePaddingH)"
        )
    }

    /// The live account's file, read on first use. A switch writes what the
    /// last account had pending and reads the new one's.
    private func current() -> UUID? {
        guard let active = AccountManager.shared.activeId else { return nil }
        if active != account {
            flush()
            account = active
            file = FrameHeightFile.load(account: active)
        }
        return active
    }

    func height(_ key: String, signature: UInt64) -> CGFloat? {
        guard current() != nil, let file else { return nil }
        let c = conditions
        guard c.width > 0, file.holds(build: c.build, width: c.width, metrics: c.metrics) else {
            return nil
        }
        return file.height(key, signature: signature).map { CGFloat($0) }
    }

    func record(_ key: String, height: CGFloat, signature: UInt64) {
        guard current() != nil else { return }
        let c = conditions
        guard c.width > 0 else { return }
        if file?.holds(build: c.build, width: c.width, metrics: c.metrics) != true {
            // Taken under other conditions: none of it answers this one.
            file = FrameHeightFile(build: c.build, width: c.width, metrics: c.metrics)
        }
        file?.record(
            key, height: Double(height), signature: signature,
            at: Date().timeIntervalSince1970)
        guard save == nil else { return }
        save = Task { [weak self] in
            try? await Task.sleep(for: Self.saveDelay)
            guard !Task.isCancelled else { return }
            self?.flush()
        }
    }

    /// Write what is pending now.
    func flush() {
        save?.cancel()
        save = nil
        guard let account, let file else { return }
        Self.writer.async { try? FrameHeightFile.write(file, account: account) }
    }

    /// An account removed or reconnected: its heights go with it.
    func forget(account id: UUID) {
        if id == account {
            save?.cancel()
            save = nil
            file = nil
            account = nil
        }
        Self.writer.async { FrameHeightFile.forget(account: id) }
    }

    /// The launch sweep: files of accounts no longer here.
    static func sweep(keeping accounts: Set<UUID>) {
        writer.async { FrameHeightFile.sweep(keeping: accounts) }
    }
}
