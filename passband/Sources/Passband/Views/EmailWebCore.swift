// THE READING SURFACE, minus the platform. Everything a rendered email is made
// of lives here — the body-preparation pipeline, the five sandbox layers wired
// into a WKWebViewConfiguration, the injected measuring script, the message
// relay and the live-frame pool — and the ONLY thing left outside is the
// representable that hands a WKWebView to a view system: `EmailWebView.swift`
// (AppKit) and `PassbandiOS/Views/EmailWebViewiOS.swift` (UIKit). Both define
// `EmailWebViewRepresentable` with the same initializer, so the SwiftUI view
// below is one view on both platforms rather than a pair that can drift.
//
// Five independent sandbox layers (page JS off, injected CSP, navigation
// refused, non-persistent store, images proxied through our own fetch path):
// see docs/SECURITY.md. The frame is sized to its measured content and never
// scrolls itself; the thread column is the one scroll.

import SwiftUI
import WebKit

struct EmailWebView: View {
    let html: String
    /// Stable id (message id) for the height memory.
    let cacheKey: String?
    /// The sender is somebody the user has written to (`sender_known` on the
    /// wire), so this body's tracking pixels are left in the document: a trusted
    /// correspondent is allowed to learn the mail was opened. Everyone else is
    /// stripped exactly as before, and the default is the strict one.
    let allowTrackers: Bool
    /// This message's parts, for the `cid:` references in the body. Empty is a
    /// real answer, not a missing one: a body with nothing to resolve against
    /// drops every cid reference it carries, which is what sealed mail wants.
    let attachments: [Attachment]
    /// WHAT THIS BODY'S TEXT IS GUESSED TO DRAW AS (MinimapGeometry.textHeight),
    /// or 0 from a caller with no guess to offer.
    ///
    /// It is the height the frame is given until the document measures itself,
    /// and that is a scrolling concern, not a cosmetic one: a message that
    /// mounts at a flat placeholder and then snaps to its real height moves
    /// every message after it by the difference, under somebody who is in the
    /// middle of reading. A guess that is roughly right turns that shove into a
    /// nudge.
    let textHeight: CGFloat
    /// Drawn as a CHAT BUBBLE rather than a card: the document hugs its
    /// content instead of filling the measure, its padding is a bubble's, and
    /// the speech tail is hung under its own outer corner — here, because the
    /// tail belongs to the white document and nothing else in this view (the
    /// image bar above it, the quoted-history chip below) is the bubble.
    let bubble: Bool
    /// Which side the bubble is on. Only read when `bubble` is set.
    let mine: Bool
    /// The message this body is, for the reader's find (ReaderFind). nil for a
    /// body no find reaches: a preview, a reveal, the forward composer.
    let findID: Int?

    @Environment(Prefs.self) private var prefs
    @Environment(AppStore.self) private var store
    /// Present only inside the reader, and read through `findRequest`.
    @Environment(ReaderFind.self) private var find: ReaderFind?

    @State private var height: CGFloat = 0
    /// The width the document came to, for a hugging bubble. 0 until the
    /// document says, and read through `displayWidth`.
    @State private var width: CGFloat = 0
    /// The collapsed document has NOTHING VISIBLE in it — a reply whose whole
    /// body is the quoted chain, a forward with no note on top. The frame
    /// collapses to nothing and the quoted-history chip stands alone; the
    /// alternative is a blank white box that says the mail is broken.
    @State private var empty = false
    /// The document's own background, for the tail. Mail brings its own page
    /// colour — a grey newsletter page with a white card in it is common —
    /// and a white tail under a grey bubble is a white tail under a grey
    /// bubble. White until the document says otherwise.
    @State private var background: Color = .white
    @State private var optedIn = false
    @State private var hasQuoted = false
    @State private var quotedHidden = true
    @State private var measured = false
    @State private var sizing: SizingPhase = .opening
    /// Whether the current sizing round was started by the READER doing
    /// something — see `apply`, which defers to the measuring pass except when
    /// this is set.
    @State private var sizingIsUserDriven = false
    /// Where the document says the active find match is: its top edge and its
    /// height, in the frame's own points. Only drawn while this body holds the
    /// cursor, so a stale one costs nothing.
    @State private var findSpot: FindSpot?

    /// HOW MANY OF THIS DOCUMENT'S REPORTED HEIGHTS ARE ALLOWED TO BECOME THE
    /// FRAME'S HEIGHT, and when. Exactly two, and both at a moment that means
    /// something.
    ///
    /// The frame's height is the document's layout viewport, so applying every
    /// report is a closed loop for any body whose height depends on the
    /// viewport (see `EmailFrameCoordinator.document`). Applying two bounds the
    /// error at one round trip — measured, that is 28 points — instead of
    /// letting it run at 840 points a second.
    ///
    /// `opening` takes the first thing the document says, which is what gets a
    /// message on screen at roughly the right size. Then nothing is applied
    /// until WebKit reports the navigation finished, which is AFTER its images,
    /// at which point the host asks the document once what it came to and stops
    /// listening. A user action that genuinely changes the content — unfolding
    /// the quoted history, letting the remote images in — puts it back to
    /// `opening`, because that is a new document to size.
    private enum SizingPhase { case opening, waiting, settling, done }

    /// The tracker-strip + dedupe + link-extraction + image-proxy pass for THIS
    /// body, warmed off the main actor at prefetch time (ThreadPrefetch) and read
    /// back in `init` — the Coordinator refuses to load a placeholder, so an
    /// unprepared body is a blank frame. The `.task` below is the cold path.
    @State private var prepared: Prepared
    /// The width the frame was last laid out at — see the geometry hook in
    /// `body`. 0 until it has been laid out once.
    @State private var frameWidth: CGFloat = 0

    /// `prepared` is seeded from the warm cache HERE: `@State` cannot be assigned
    /// from `body`, and every later hook is a frame too late.
    init(
        html: String, cacheKey: String? = nil, allowTrackers: Bool = false,
        attachments: [Attachment] = [], textHeight: CGFloat = 0,
        bubble: Bool = false, mine: Bool = false, findID: Int? = nil
    ) {
        self.html = html
        self.cacheKey = cacheKey
        self.allowTrackers = allowTrackers
        self.attachments = attachments
        self.textHeight = textHeight
        self.bubble = bubble
        self.mine = mine
        self.findID = findID
        _prepared = State(
            initialValue: PreparedBodies.shared.get(
                PreparedKeyMemo.shared.key(
                    message: cacheKey, html: html, allowTrackers: allowTrackers,
                    attachments: attachments)) ?? .empty)
    }

    struct Prepared: Equatable, Sendable {
        var sourceHash: Int
        var html: String
        /// Tracking pixels the strip pass FOUND. Removed from `html` unless
        /// `trackersAllowed`, in which case they are still in the document and
        /// this is only what the badge reports.
        var trackers: Int
        var trackersAllowed: Bool
        var hasRemoteCandidates: Bool
        /// The ORIGINAL http(s) urls behind this body's proxied references, in
        /// document order, capped (ImageProxy.maxWarmURLs) — what the launch
        /// warmer pre-fetches and pins, not the full uncapped render set.
        var imageURLs: [String]
        /// The mail's TOPLINE — its `<title>`, which the daemon's sanitizer
        /// leaves as bare text at the top of the body when it drops the head
        /// around it (Topline). Taken OUT of `html` and shown above the
        /// frame instead, muted, where a caption goes.
        var topline: String?

        static let empty = Prepared(
            sourceHash: 0, html: "", trackers: 0, trackersAllowed: false,
            hasRemoteCandidates: false, imageURLs: [], topline: nil)

        static func make(
            from html: String, allowTrackers: Bool = false, attachments: [Attachment] = []
        ) -> Prepared {
            // The pass runs either way — one regex sweep, and its COUNT is what
            // the badge reports — but a known sender's body keeps its pixels.
            // ORDER MATTERS when it does strip: trackers come out FIRST, so a
            // tracking pixel can never be the "first occurrence" that suppresses
            // a real image further down the thread. Dedupe layers on top, never
            // in place of it.
            // The topline comes off FIRST: it is text ahead of the document,
            // not part of it, and every pass below is about the document.
            let (topline, source) = Topline.split(html)
            let stripped = Trackers.strip(source)
            let body = allowTrackers ? source : stripped.html
            // A repeated sender logo is still part of THIS message. Suppressing
            // it because an older message used the same URL made current mail
            // look broken (and could erase every image in a short reply). Keep
            // the within-body pass for quoted-history duplicates, but do not let
            // an earlier message remove content from this one.
            let deduped = ImageRepeats.dropRepeats(body)
            // Read off the DEDUPED html: a message whose only images were
            // repeats has nothing left to fetch, so it must not offer the
            // "load remote images" bar.
            let hasRemoteCandidates = Trackers.hasNetworkImages(deduped)
            // BETWEEN the two, and it has to be: the dedupe never touches a cid
            // reference (they are message-specific, not the repeated chrome it
            // removes), while the proxy must not see what this mints — a
            // `passband-cid:` src is not http(s), so it would be left alone, but
            // running before it is what keeps that a fact rather than a
            // coincidence of ImageProxy's prefix test.
            let inlined = CidImages.rewrite(html: deduped, attachments: attachments)
            // LAST, after the read above: the rewrite replaces every http(s)
            // image reference with a `passband-img:` one, which those scans would
            // no longer recognise as remote.
            let proxied = ImageProxy.rewrite(inlined)
            return Prepared(
                sourceHash: cacheKey(html, allowTrackers, attachments),
                html: proxied.html,
                trackers: stripped.blocked,
                trackersAllowed: allowTrackers,
                hasRemoteCandidates: hasRemoteCandidates,
                imageURLs: proxied.urls,
                topline: topline)
        }

        /// Tracker policy is part of the identity: the two policies produce
        /// different documents from the same body.
        ///
        /// So are the PARTS, for the same reason: the cid rewrite resolves
        /// against them, so one body quoted into two messages carrying different
        /// attachments prepares into two different documents. Everything the
        /// rewrite reads is folded in — the id and content-id it matches on, and
        /// the three fields of the inline gate — because a hit on a stale key
        /// would paste one message's photo into another's.
        static func cacheKey(
            _ html: String, _ allowTrackers: Bool, _ attachments: [Attachment]
        ) -> Int {
            var hasher = Hasher()
            hasher.combine(html)
            hasher.combine(allowTrackers)
            for att in attachments {
                hasher.combine(att.id)
                hasher.combine(att.content_id)
                hasher.combine(att.downloadable)
                hasher.combine(att.mime)
                hasher.combine(att.size)
            }
            return hasher.finalize()
        }
    }

    private var allowRemote: Bool { prefs.loadRemoteImages || optedIn }

    /// Spin WebKit up BEFORE the first email is opened. Two separate costs are
    /// being paid here, and only one of them used to be: the FIRST WKWebView the
    /// app ever builds pays a one-off framework initialisation (measured at
    /// 53-58ms), and EVERY new frame pays some seventy milliseconds of content
    /// process launch before it can draw anything.
    ///
    /// It used to be a throwaway frame, which bought the first of those and
    /// nothing else — the first message opened still faced a blank box while its
    /// own process came up. The warmer is a SPARE instead: one real frame, the
    /// same cost, held in the drawer the reader takes from. Idempotent.
    @MainActor
    static func warmProcess() {
        WebFramePool.shared.seedSpare(EmailWebViewRepresentable.buildSpare)
    }

    /// Frame height shown before the first successful measurement, for a body
    /// whose caller offered no guess at all. A LAST RESORT: a frame that opens
    /// at this and measures at ten times it is the thread lurching.
    private static let placeholderHeight: CGFloat = 120
    /// The document's own top and bottom padding (the body padding in
    /// `document`), which a caller counting lines of text knows nothing about.
    /// A bubble is padded like the plain-text bubble beside it, a card like a
    /// card. Read off the same metrics the stylesheet is written in, so the
    /// guess opens at the padding the density actually draws.
    private var documentPadding: CGFloat {
        let metrics = ReaderMetrics.current
        return 2 * (bubble ? metrics.bubblePaddingV : metrics.cardPadding)
    }
    /// Height used when measurement never arrives at all — generous on purpose:
    /// too tall leaves whitespace, too short silently hides mail.
    private static let unmeasuredFallbackHeight: CGFloat = 900

    var body: some View {
        // A sent bubble's chrome — the image bar, the quoted-history chip —
        // keeps to the bubble's own side, or a short bubble would sit at the
        // left of a stack the chip had made wider and end up flush with nothing.
        VStack(alignment: bubble && mine ? .trailing : .leading, spacing: Space.s8) {
            imageBar
            // The mail's own topline, where the sender put it — above the
            // body — but as a caption rather than the first line of the
            // page: grey, small, and not part of the bubble.
            if let topline = prepared.topline {
                Text(topline)
                    .font(Typo.micro)
                    .foregroundStyle(Palette.inkFaint)
                    .lineLimit(2)
                    .multilineTextAlignment(bubble && mine ? .trailing : .leading)
                    .textSelection(.enabled)
                    .padding(.horizontal, Space.s2)
            }

            EmailWebViewRepresentable(
                html: prepared.html,
                source: prepared.sourceHash,
                allowRemote: allowRemote,
                collapseQuotes: quotedHidden,
                bubble: bubble,
                // The pool's message identity. nil is not "unkeyed", it is
                // NOT POOLABLE — see WebFramePool.Key.
                poolKey: cacheKey,
                find: findRequest,
                onFind: { request, result in noteFind(request, result) },
                onHeight: { m in
                    apply(m)
                },
                onLoaded: {
                    // The document and its subresources are in. Whatever it
                    // says next is the size it came to; after that the frame
                    // stops being a party to its own layout.
                    if sizing == .waiting { sizing = .settling }
                },
                onQuotedFound: { hasQuoted = $0 },
                onLink: { Opener.open($0) }
            )
            // `maxWidth`, not `width`: a hugging bubble is capped at what the
            // document reported, and the column caps that in turn, so a
            // measurement taken in a wider window can never push a bubble
            // past the edge while the heights are being retaken.
            .frame(maxWidth: displayWidth)
            .frame(height: empty ? 0 : displayHeight)
            .overlay(alignment: .topLeading) { findAnchor }
            // A NARROWER OR WIDER FRAME IS A NEW DOCUMENT TO SIZE. The column
            // changes width when a pane opens beside the reader or the window
            // is resized; the document reflows and reports its new height —
            // and a machine already in `.done` ignored it, so the frame kept
            // the old width's height (clipped, or trailing white space) until
            // the offscreen pass got round to it. Re-opening the sizing takes
            // the reflowed height at once. A card only: a bubble's frame
            // narrows to what its own document reported, which is not news.
            .onGeometryChange(for: CGFloat.self) { $0.size.width } action: { width in
                let previous = frameWidth
                frameWidth = width
                guard !bubble, previous > 0, abs(width - previous) > 0.5, measured,
                    sizing != .opening
                else { return }
                // Straight to `.settling`, not back to `.opening`: a reflow
                // starts no navigation, so nothing would ever move the machine
                // on from `.waiting` and the document's final word would be
                // dropped. Settling takes the next report and stops, and the
                // offscreen pass (keyed on the width) re-measures behind it.
                sizing = .settling
                sizingIsUserDriven = false
            }
            .clipShape(RoundedRectangle(cornerRadius: 10, style: .continuous))
            // The speech tail, on the DOCUMENT's outer bottom corner, in the
            // document's own page colour. BEHIND the frame, not over it: the
            // tail's solid span reaches well inside the bubble to bury the
            // clipped corner, and the document is opaque, so from behind only
            // the poke ever shows — over the frame the span showed as a
            // block of the wrong colour on any mail whose page is not white.
            // Mounted in both styles and merely clear in a card, so a style
            // flip changes no identity.
            .background(alignment: mine ? .bottomTrailing : .bottomLeading) {
                BubbleTail(mine: mine)
                    .fill(background)
                    .frame(width: 12, height: 12)
                    .offset(x: mine ? BubbleTail.poke : -BubbleTail.poke)
                    .opacity(bubble && !empty ? 1 : 0)
                    .allowsHitTesting(false)
            }
            // PAINT-HOLD: the frame keeps its space but stays invisible until the
            // first measurement lands, so no half-laid-out document snaps to size.
            // WebKit's `suppressesIncrementalRendering` is deliberately NOT used:
            // it waits for every subresource, holding parsed mail hostage to the
            // slowest CDN. Measurement fires at document end, before images.
            .opacity(measured ? 1 : 0)

            // Under the mail: the quoted-history toggle and the tracker
            // footnote, on one line when both are here. Conditional as a
            // whole, or an empty row would still cost the stack's spacing.
            if hasQuoted || (prepared.trackers > 0 && !prepared.trackersAllowed) {
              HStack(spacing: Space.s10) {
                if hasQuoted {
                    Button {
                        quotedHidden.toggle()
                    } label: {
                        Text(quotedHidden ? "··· show quoted history" : "hide quoted history")
                            .font(Typo.micro)
                            .padding(.horizontal, Space.scaled(9))
                            .padding(.vertical, Space.scaled(3))
                    }
                    // Text-on-hover like the header actions: the only control
                    // INSIDE the reading surface, where a glass pill reads as
                    // chrome.
                    .buttonStyle(.textAction)
                    .help("the quoted reply chain below this message")
                }
                trackerNote
              }
            }
        }
        .onAppear {
            // Before the memory is read: a size filed at the other density is
            // a frame that opens wrong and then snaps (see ReaderMetrics.sync).
            ReaderMetrics.sync()
            if let cacheKey, let remembered = FrameHeights.shared.get(cacheKey) {
                height = remembered
            }
            if bubble, let cacheKey, let remembered = FrameHeights.shared.hugWidth(cacheKey) {
                width = remembered
            }
        }
        // A NEW DOCUMENT TO SIZE. Unfolding the quoted history and letting the
        // remote images in are the two things that legitimately change what
        // this message is; everything else that reaches the frame is the frame
        // talking to itself.
        .onChange(of: quotedHidden) { _, _ in restartSizing(userDriven: true) }
        .onChange(of: allowRemote) { _, _ in restartSizing(userDriven: true) }
        // And a different BODY in the same frame, which is not a user action
        // but is still a new document to size: the cold path finishing its
        // preparation, or this view being reused for another message as the
        // stack recycles. Without it the new document's first measurement
        // arrives into a machine that has already stopped listening.
        .onChange(of: prepared.sourceHash) { _, _ in restartSizing(userDriven: false) }
        // A DENSITY FLIP RESTYLES THIS DOCUMENT IN PLACE: the coordinator swaps
        // the app's own stylesheet under the live document (ReaderMetrics)
        // rather than reloading it, so the frame keeps its content process, its
        // images and its quoted-history state. What it cannot keep is its size.
        // Every height on file was taken at the old padding and type, so the
        // file goes first, and a hugging bubble lets go of its old width so it
        // can come to the new one. A settled machine goes straight to
        // `.settling` for the reason a reflow does: no navigation starts, so
        // nothing would ever move it on from `.waiting`. One still opening is
        // left alone — the restyle lands before its own settle (see
        // `EmailFrameCoordinator.documentDidLoad`).
        .onChange(of: prefs.density) { _, _ in
            ReaderMetrics.sync()
            width = 0
            if sizing == .done {
                sizing = .settling
                sizingIsUserDriven = false
            }
        }
        // COLD PATH ONLY — `init` already seeded `prepared` from the warmer; this
        // runs for a body opened before its thread warmed (or since evicted). Off
        // the main actor: the scans are a full regex walk of the body.
        .task(id: preparedKey) {
            guard prepared.sourceHash != preparedKey else { return }
            let key = preparedKey
            if let warm = PreparedBodies.shared.get(key) {
                prepared = warm
                return
            }
            let source = html
            let allow = allowTrackers
            let parts = attachments
            let made = await Task.detached(priority: .userInitiated) {
                Prepared.make(from: source, allowTrackers: allow, attachments: parts)
            }.value
            PreparedBodies.shared.set(key, made)
            guard !Task.isCancelled else { return }
            prepared = made
        }
        // SAFETY NET: the view is sized purely from what the measuring script
        // reports, so if that never arrives fall back to a tall box rather than
        // leaving the message clipped at the placeholder height.
        .task(id: prepared.sourceHash) {
            try? await Task.sleep(for: .seconds(2))
            guard !Task.isCancelled, !measured else { return }
            // A REMEMBERED MEASUREMENT STANDS: the frame is already that size,
            // so it becomes visible without also lurching — which is what
            // reaching straight for the fallback did to every message that had
            // a perfectly good size on file.
            //
            // With nothing measured, the tall fallback still wins, and it wins
            // over the guess too. A frame that never reported is a frame
            // nothing verified, and the guess counts text and scores pictures
            // at zero: too tall leaves whitespace, too short silently hides
            // mail, and only one of those is recoverable by scrolling.
            if height == 0 {
                height = rememberedHeight ?? max(guessedHeight ?? 0, Self.unmeasuredFallbackHeight)
            }
            measured = true
        }
    }

    /// What the reader's find wants of this body, or nil when there is no
    /// find, or no reader.
    private var findRequest: FindRequest? {
        guard let find, let findID else { return nil }
        return find.request(for: findID)
    }

    /// The document's answer: file the count, and when this body holds the
    /// cursor, remember where the match is and ask the reader to look.
    private func noteFind(_ request: FindRequest, _ result: FindResult) {
        guard let find, let findID else { return }
        find.report(findID, query: request.query, count: result.count, active: result.active)
        guard result.active >= 0 else { return }
        findSpot = FindSpot(y: result.y, height: max(1, result.height))
        find.aim()
    }

    /// THE SCROLL TARGET FOR THE ACTIVE MATCH. The column scrolls, the frame
    /// does not, so "bring that match into view" has to be said to the column,
    /// and the column can only aim at a view. This is that view: nothing
    /// drawn, pinned where the document says the match is.
    @ViewBuilder
    private var findAnchor: some View {
        if let request = findRequest, request.active >= 0, let spot = findSpot {
            FindAnchor(spot: spot)
        }
    }

    /// A new document to size, and whether the reader asked for it.
    private func restartSizing(userDriven: Bool) {
        sizing = .opening
        sizingIsUserDriven = userDriven
    }

    /// Take a reported measurement, or decline it. THE ONE PLACE a measurement
    /// ever becomes the frame's size.
    private func apply(_ m: FrameMeasurement) {
        let h = m.height
        guard h > 0, h <= Self.sanityCeiling else {
            trace("refused \(Int(h))")
            return
        }
        // Everything below may file a size, and the file has to be for the
        // density the document is now drawn at.
        ReaderMetrics.sync()
        // THE WIDTH AND THE EMPTINESS ARE TAKEN FROM EVERY REPORT, ahead of
        // the phases below, which exist for the height alone. The height is
        // the viewport-feedback problem (a `vh` body is as tall as the box it
        // is asked in); the width has no such loop — a hugging document is
        // min(its widest line, the frame), so narrowing the frame to what it
        // reported reports the same number back — and the offscreen pass never
        // measures either, so a frame deferring to that pass would otherwise
        // never learn them.
        empty = m.empty
        if let page = m.background { background = page }
        // A floor under the hug: nothing this narrow is a message, so a
        // report like that is a document mid-layout, not a size.
        if bubble, m.width >= 40 {
            width = m.width
            if let cacheKey { FrameHeights.shared.setHugWidth(cacheKey, m.width) }
        }
        // A MEASUREMENT TAKEN AT A FIXED VIEWPORT BEATS ANYTHING THIS FRAME CAN
        // WORK OUT, and for one kind of body it is the only real answer there
        // is. A document written in `vh` units has no intrinsic height — it is
        // one viewport tall, whatever viewport you ask at, so `h = h + padding`
        // has no solution — and a frame deriving it from its own box just
        // returns wherever it started plus a couple of round trips. Measured:
        // opened at the placeholder, a full-height email settled at 176 points
        // against 828 for the same document asked at a real window.
        //
        // Not for a user action, though: unfolding the quoted history or
        // letting the images in makes this a different document from the one
        // the pass measured, and then the frame in front of somebody is the
        // only thing that knows.
        if !sizingIsUserDriven, let key = cacheKey,
            let measuredHeight = FrameHeights.shared.authoritative(key)
        {
            height = measuredHeight
            measured = true
            sizing = .done
            trace("deferred to measured \(Int(measuredHeight)), offered \(Int(h))")
            return
        }
        switch sizing {
        case .opening: sizing = .waiting
        case .settling: sizing = .done
        // Between the opening measurement and the navigation finishing, the
        // document is still arriving and every report it makes is a report
        // about a frame we sized from its last one. Nothing to learn there.
        case .waiting, .done:
            trace("ignored \(Int(h)) in \(sizing)")
            return
        }
        trace("applied \(Int(height)) -> \(Int(h)) (\(sizing))")
        height = h
        measured = true
        // The memory stores the DEFAULT (collapsed) state only — and stores
        // this as PROVISIONAL, because it was taken inside a box this frame had
        // sized itself. It holds the message at a sane size until the pass
        // reaches it, and never displaces what the pass finds.
        if let cacheKey, quotedHidden { FrameHeights.shared.set(cacheKey, h) }
    }

    /// Nothing this tall is an email. A number past here is a runaway — the
    /// frame growing on its own layout — and letting one reach the cache is
    /// what used to make a poisoned message stay poisoned for the session.
    private static let sanityCeiling: CGFloat = 60_000

    /// EVERY HEIGHT THIS FRAME IS OFFERED, and what became of it, in developer
    /// mode. A message resizing itself is the one bug here that is invisible
    /// from the outside until it is enormous, and a line per decision is the
    /// difference between seeing it happen and theorising about it: a healthy
    /// message prints two `applied` lines and stops, and a message fighting its
    /// own layout prints a wall of `ignored` ones climbing by 28.
    private func trace(_ what: String) {
        guard prefs.developerMode, let cacheKey else { return }
        print("[passband.height] msg \(cacheKey) \(what)")
    }

    /// Memoized per message: the raw key is a hash of the whole html, and this
    /// is read from `body` (as a task id) on every evaluation.
    private var preparedKey: Int {
        PreparedKeyMemo.shared.key(
            message: cacheKey, html: html, allowTrackers: allowTrackers, attachments: attachments)
    }
    private var rememberedHeight: CGFloat? { cacheKey.flatMap { FrameHeights.shared.get($0) } }
    /// What the caller's line count says this document will come to, or nil
    /// when it offered none. Never remembered as a height: it is a guess, and
    /// `FrameHeights` holds measurements.
    private var guessedHeight: CGFloat? {
        textHeight > 0 ? textHeight + documentPadding : nil
    }
    /// How wide the frame may be: what the document said, or what it said
    /// last time this message was open, or the whole measure. A card is
    /// always the whole measure.
    private var displayWidth: CGFloat {
        guard bubble else { return .infinity }
        if width > 0 { return width }
        return cacheKey.flatMap { FrameHeights.shared.hugWidth($0) } ?? .infinity
    }
    /// THE CONFIDENCE ORDER, and it is not the obvious one: a measurement taken
    /// at a FIXED viewport outranks what this frame worked out for itself, even
    /// though this frame is the one on screen. It has to, because what this
    /// frame worked out was worked out inside a box sized from its own last
    /// answer — fine for ordinary mail, meaningless for a body written in `vh`
    /// units, which is one viewport tall whatever viewport you ask at.
    ///
    /// Reading it here rather than only at mount is what lets a message adopt
    /// its size the moment the pass measures it. The newest message is always
    /// on screen before that pass has started, so it would otherwise be the one
    /// message that never gets a real one.
    ///
    /// UNLESS THE READER CHANGED THE DOCUMENT — unfolded the quoted history,
    /// let the images in — in which case what is on screen is no longer what
    /// was measured, and the frame in front of somebody is the only thing that
    /// knows how tall it is.
    private var displayHeight: CGFloat {
        if !sizingIsUserDriven, let key = cacheKey,
            let measuredHeight = FrameHeights.shared.authoritative(key)
        {
            return measuredHeight
        }
        return height > 0 ? height : (rememberedHeight ?? guessedHeight ?? Self.placeholderHeight)
    }

    @ViewBuilder
    private var imageBar: some View {
        if !allowRemote && prepared.hasRemoteCandidates {
            Button {
                optedIn = true
            } label: {
                Label("remote images blocked — load for this email", systemImage: "photo")
                    .font(Typo.micro)
                    .padding(.horizontal, Space.scaled(9))
                    .padding(.vertical, Space.s4)
            }
            .buttonStyle(.glass)
            .foregroundStyle(Palette.inkFaint)
            .help(
                "remote images are off by default (Settings → Mail); load them for this email only")
        } else if prepared.trackers > 0, prepared.trackersAllowed {
            // The count is honest about what was LEFT IN: this sender is someone
            // the user writes to, so they are allowed to see the open. Muted, not
            // green — nothing was protected here.
            Label(
                "trackers allowed (known sender)",
                systemImage: "eye"
            )
            .font(Typo.micro)
            .foregroundStyle(Palette.inkFaint)
            .padding(.horizontal, Space.scaled(9))
            .padding(.vertical, Space.s4)
            .glassCapsule(interactive: false)
            .help(
                "\(prepared.trackers) tracking pixel\(prepared.trackers == 1 ? "" : "s") left in place: you have emailed this sender, so they may see that you opened this"
            )
        }
    }

    /// A FOOTNOTE UNDER THE MAIL, not a badge over it. This is the normal case
    /// — nearly every commercial mail carries a pixel and nearly every one is
    /// stripped — and a green capsule over every bubble was the reader being
    /// congratulated for the default. Faint, small, and after the mail, where
    /// a note about how it was rendered belongs; the help text carries the
    /// detail. The two things that DO warrant attention before reading — the
    /// remote-images offer and pixels left in for a known sender — stay above.
    @ViewBuilder
    private var trackerNote: some View {
        if prepared.trackers > 0, !prepared.trackersAllowed {
            Label(
                "\(prepared.trackers) tracker\(prepared.trackers == 1 ? "" : "s") blocked",
                systemImage: "eye.slash"
            )
            .font(Typo.micro)
            .foregroundStyle(Palette.inkFaint)
            .padding(.horizontal, Space.s2)
            .help(
                "tracking pixels removed before render; links open externally and no referrer is ever sent"
            )
        }
    }

}

// MARK: - the frame, before a view system touches it

/// Everything about a mail frame that has no opinion about AppKit or UIKit: the
/// one ephemeral data store, the sandbox configuration built on top of it, the
/// injected measuring script, and the pool identity of a rendered document. The
/// two representables differ only in what they wrap this in.
enum EmailFrame {
    /// ONE ephemeral data store shared by every email view. `nonPersistent()`
    /// mints a BRAND NEW store per call, so a per-message store cached nothing —
    /// every image re-fetched on every open. Sharing restores the URL cache while
    /// keeping what matters: nothing touches disk, no cookie jar survives.
    @MainActor
    static let sharedDataStore: WKWebsiteDataStore = .nonPersistent()

    /// Every email frame answers `prefers-color-scheme: light`, whatever the
    /// app's appearance. The canvas `document` paints is ALWAYS white, and a
    /// frame that inherited dark mode would fire the sender's dark stylesheet
    /// on top of it. Greptile's is the case that caught it: its dark rules turn
    /// the copy #FEFEFE and darken only `body`, while the inline #FEFEFE on
    /// its wrapper div stays put, so the whole message was white on white. The
    /// measuring frame gets the same pin, because the dark rules can also swap
    /// which logo is displayed and so change the height.
    @MainActor
    static func pinLightAppearance(_ view: WKWebView) {
        #if os(macOS)
            view.appearance = NSAppearance(named: .aqua)
        #else
            view.overrideUserInterfaceStyle = .light
        #endif
    }

    /// A configuration with all five layers wired, plus the relay that will be
    /// the built frame's permanent hinge. Returned together because the relay is
    /// registered INTO the configuration — a frame cannot be handed one later.
    @MainActor
    static func makeConfiguration() -> (WKWebViewConfiguration, FrameRelay) {
        let config = WKWebViewConfiguration()

        // LAYER 2: page content cannot execute script, whatever it contains.
        // Our injected user script is governed separately and still runs.
        config.defaultWebpagePreferences.allowsContentJavaScript = false
        // LAYER 5: no cookie jar, no persistent storage, nothing survives close.
        config.websiteDataStore = sharedDataStore
        // Images are the ONE resource a body may load, and they ride our handler
        // rather than WebKit's loader; one shared handler keeps the in-flight
        // bookkeeping in a single place. TWO schemes reach it: remote art
        // rewritten by ImageProxy, and this message's own attachment parts
        // rewritten by CidImages. Both configurations register both — a frame
        // that knew only one would draw the other's images as broken glyphs.
        config.setURLSchemeHandler(ImageSchemeHandler.shared, forURLScheme: ImageProxy.scheme)
        config.setURLSchemeHandler(ImageSchemeHandler.shared, forURLScheme: CidProxy.scheme)

        // The relay, not the coordinator, is wired to the frame — it outlives
        // every coordinator that borrows it. See FrameRelay.
        let relay = FrameRelay()
        let controller = WKUserContentController()
        controller.add(relay, name: FrameRelay.name)
        controller.addUserScript(
            WKUserScript(
                source: measuringScript, injectionTime: .atDocumentEnd,
                forMainFrameOnly: true))
        // Find in the open email (Lib/FindInThread.swift). Its own script so
        // the search can be read, and tested, without the measuring bridge.
        controller.addUserScript(
            WKUserScript(
                source: FindScript.source, injectionTime: .atDocumentEnd,
                forMainFrameOnly: true))
        config.userContentController = controller

        return (config, relay)
    }

    /// What a LIVE frame would have to be holding to be reusable for this render,
    /// or nil if this render cannot use one (nothing to render yet, or no message
    /// id). At the density in force now: a frame parked under the other one is
    /// a miss, not a frame to adopt and then restyle (see `WebFramePool.Key`).
    @MainActor
    static func renderKey(
        poolKey: String?, html: String, source: Int, allowRemote: Bool, bubble: Bool
    ) -> WebFramePool.Key? {
        guard let poolKey, !html.isEmpty else { return nil }
        return WebFramePool.Key(
            message: poolKey, allowRemote: allowRemote,
            document: EmailFrameCoordinator.documentHash(source: source, bubble: bubble),
            metrics: .current)
    }

    /// Injected AFTER document end (the message cannot run script to interfere):
    /// collapses trailing quoted history, reports document height on load / each
    /// image settling / a ResizeObserver tick, reports link clicks up to native
    /// rather than navigating, and exposes the collapse toggle the host calls.
    static let measuringScript = """
        (function () {
          var send = function (payload) {
            try { window.webkit.messageHandlers.passband.postMessage(payload); } catch (e) {}
          };

          // Declared UP HERE because the quoted-history collapse below calls
          // measure() before the height section: an undefined `last` makes the
          // comparison NaN and throws away the measurement that sizes the frame.
          var last = -1;

          // ---- quoted history --------------------------------------------
          // Mirrors Quotes.swift: the first TOP-LEVEL <blockquote> after which
          // the document has no substantial text of its own anchors the history,
          // so a bottom-posted reply never qualifies.
          var ATTRIBUTION = [
            /^On .{1,200} wrote:\\s*$/,
            /^-{2,}\\s*(original|forwarded) message\\s*-{0,}$/i,
            /^Begin forwarded message:?\\s*$/i
          ];
          function isAttribution(t) {
            t = (t || '').trim();
            if (!t) return false;
            return ATTRIBUTION.some(function (re) { return re.test(t); });
          }
          function textLengthAfter(root, node) {
            var len = 0;
            var walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
            var t;
            while ((t = walker.nextNode())) {
              if (node.contains(t)) continue;
              if (node.compareDocumentPosition(t) & Node.DOCUMENT_POSITION_FOLLOWING) {
                len += (t.textContent || '').trim().length;
              }
            }
            return len;
          }
          function findQuoteNodes() {
            var body = document.body;
            if (!body) return [];
            var quotes = Array.prototype.slice
              .call(body.querySelectorAll('blockquote'))
              .filter(function (b) {
                return !(b.parentElement && b.parentElement.closest('blockquote'));
              });
            for (var i = 0; i < quotes.length; i++) {
              var q = quotes[i];
              if (textLengthAfter(body, q) > 200) continue;
              var nodes = [];
              var prev = q.previousElementSibling ||
                (q.parentElement && q.parentElement.previousElementSibling);
              if (prev && (prev.textContent || '').trim().length < 300 &&
                  isAttribution((prev.textContent || '').replace(/\\s+/g, ' '))) {
                nodes.push(prev);
              }
              nodes.push(q);
              for (var j = 0; j < quotes.length; j++) {
                var r = quotes[j];
                if (r !== q &&
                    (q.compareDocumentPosition(r) & Node.DOCUMENT_POSITION_FOLLOWING)) {
                  nodes.push(r);
                }
              }
              return nodes;
            }
            return [];
          }

          // ---- full-bleed mail -------------------------------------------
          // A newsletter paints its own page: one wrapper (a table, a div)
          // with an opaque background, as wide as the body. Our padding
          // around that is a ring of OUR page colour around THEIR page —
          // white around grey. So when the visible content is nothing but
          // such wrappers, the body gives up its padding and takes their
          // colour, and the tail follows (pageBackground reads the body).
          // Runs once, before the first measurement, so the frame is sized
          // to what it will show. Text mail has text at the top level or a
          // transparent wrapper, and keeps its padding.
          function opaque(bg) {
            return bg && bg !== 'transparent' && bg !== 'rgba(0, 0, 0, 0)';
          }
          function adoptFullBleed() {
            var b = document.body;
            if (!b) return;
            var kids = Array.prototype.filter.call(b.childNodes, function (n) {
              if (n.nodeType === 3) return (n.textContent || '').trim().length > 0;
              if (n.nodeType !== 1) return false;
              if (n.tagName === 'STYLE' || n.tagName === 'SCRIPT') return false;
              return n.getClientRects().length > 0;
            });
            if (!kids.length) return;
            var first = kids[0];
            if (first.nodeType !== 1) return;
            var bg = getComputedStyle(first).backgroundColor;
            if (!opaque(bg)) return;
            for (var i = 1; i < kids.length; i++) {
              var k = kids[i];
              if (k.nodeType !== 1 || getComputedStyle(k).backgroundColor !== bg) return;
            }
            var cs = getComputedStyle(b);
            var inner = b.clientWidth - parseFloat(cs.paddingLeft) - parseFloat(cs.paddingRight);
            if (inner <= 0 || first.getBoundingClientRect().width < inner * 0.95) return;
            b.style.padding = '0';
            b.style.backgroundColor = bg;
          }
          adoptFullBleed();

          // ---- hug or fill ---------------------------------------------
          // A bubble document hugs its content (body.hug, EmailFrame) ONLY
          // when the content can say how wide it is: plain prose, a few
          // inline images. Tables and percentage widths answer with nothing,
          // and anything already overflowing sideways is wider than it says.
          // Those fill the measure — which is what a 600px newsletter was
          // designed for anyway. Decided once, before the first measurement,
          // so the frame is never narrowed on a bad answer.
          function decideHug() {
            var b = document.body;
            if (!b || !b.classList.contains('hug')) return;
            var complex = document.querySelector(
              'table, center, [width$="%"], [style*="%"], iframe');
            if (complex || b.scrollWidth > b.clientWidth + 1) b.classList.remove('hug');
          }
          decideHug();

          // ---- fit to width ------------------------------------------------
          // A template built at a FIXED width (a 600px table, a 640px image
          // row) is wider than a phone, and the body is overflow:hidden, so
          // the right side was simply gone: text cut mid-word, no way to
          // scroll to it. Scale the whole document down until its widest box
          // fits, the way Mail does. Measured at zoom 1 each time, since the
          // overflow is a property of the layout, not of the last fit.
          // WebKit's zoom is the standardized kind: rects come back in visual
          // px, scrollHeight/scrollWidth in the element's own; contentHeight
          // accounts for that.
          var zoom = 1;
          function fitWidth() {
            var b = document.body, h = document.documentElement;
            if (!b || !h) return;
            h.style.zoom = '';
            zoom = 1;
            var over = b.scrollWidth, room = b.clientWidth;
            if (room > 0 && over > room + 1) {
              zoom = Math.max(0.25, room / over);
              h.style.zoom = String(zoom);
            }
          }
          var fitAt = window.innerWidth;
          window.addEventListener('resize', function () {
            if (window.innerWidth === fitAt) return;
            fitAt = window.innerWidth;
            fitWidth();
            measure();
          });

          var quoteNodes = findQuoteNodes();
          window.__passbandSetQuotes = function (collapsed) {
            for (var i = 0; i < quoteNodes.length; i++) {
              quoteNodes[i].style.display = collapsed ? 'none' : '';
            }
            // An expanded chain can be wider than the reply above it.
            fitWidth();
            measure();
          };
          // Collapsed by default, BEFORE the first measure, so the frame sizes
          // to the collapsed content and never flashes the full chain.
          window.__passbandSetQuotes(true);
          send({ kind: 'quoted', value: quoteNodes.length > 0 });

          // ---- height ------------------------------------------------------
          // Measure the CONTENT, never documentElement.scrollHeight: the root's
          // scrollHeight is floored at the VIEWPORT height, and the viewport here
          // IS the frame being sized, so a shrinking document (collapsing quoted
          // history) could never report smaller. max(scrollHeight, rect) also
          // covers children overflowing the body box (floats, positioned tables).
          function contentHeight() {
            var b = document.body;
            if (!b) {
              return document.documentElement ? document.documentElement.scrollHeight : 0;
            }
            return Math.max(
              Math.ceil(b.scrollHeight * zoom), Math.ceil(b.getBoundingClientRect().height));
          }
          // The width the body came to. Only a bubble document hugs (its body
          // is `fit-content`), so for a card this is just the frame's width.
          function contentWidth() {
            var b = document.body;
            return b ? Math.ceil(b.getBoundingClientRect().width) : 0;
          }
          // Nothing visible in the collapsed document: no text, and no image
          // that has a box (display:none ones have no client rects).
          function contentEmpty() {
            var b = document.body;
            if (!b) return true;
            if ((b.textContent || '').trim().length > 0) return false;
            return !Array.prototype.some.call(document.images, function (img) {
              return img.getClientRects().length > 0;
            });
          }
          // The page colour the tail should be: the body's, unless the sender
          // left ours (white) on the body and painted the html element.
          function pageBackground() {
            var b = document.body, h = document.documentElement;
            var bg = b ? getComputedStyle(b).backgroundColor : '';
            if ((!bg || bg === 'rgba(0, 0, 0, 0)' || bg === 'transparent') && h) {
              bg = getComputedStyle(h).backgroundColor;
            }
            return bg || '';
          }
          function measurement() {
            return {
              height: contentHeight(), width: contentWidth(), empty: contentEmpty(),
              background: pageBackground()
            };
          }
          function measure() {
            var m = measurement();
            if (m.height > 0 && Math.abs(m.height - last) > 1) {
              last = m.height;
              send({
                kind: 'height', value: m.height, width: m.width, empty: m.empty,
                background: m.background
              });
            }
          }
          // THE SAME NUMBER, ASKED FOR RATHER THAN SENT. A frame being
          // measured off screen (FrameMeasurer) loads one document after
          // another through one web view, and a pushed message carries no
          // sign of which document it came from — the previous body's height
          // lands in the next body's lap about a quarter of the time. A call
          // has an answer, and the answer belongs to the document that was
          // asked. Defined on the collapsed document, so it reports what the
          // reader will actually show.
          window.__passbandHeight = contentHeight;
          window.__passbandMeasure = measurement;
          measure();
          requestAnimationFrame(measure);
          // Images with a width attribute can widen a table once they land.
          window.addEventListener('load', function () { fitWidth(); measure(); });
          // Images arriving are the only post-load height change (no page
          // script can run) — re-measure as each settles.
          Array.prototype.forEach.call(document.images, function (img) {
            img.decoding = 'async';
            img.addEventListener('load', measure);
            img.addEventListener('error', measure);
          });
          if (window.ResizeObserver && document.body) {
            new ResizeObserver(measure).observe(document.body);
          }

          // Called by the host when a POOLED frame is handed to a new view:
          // nothing here re-runs on a document that was not reloaded, so the new
          // host knows neither the height nor whether a quoted chain exists.
          // Clearing `last` is the trick — measure() suppresses an unchanged
          // height, and a re-shown frame is exactly unchanged.
          window.__passbandResend = function () {
            send({ kind: 'quoted', value: quoteNodes.length > 0 });
            last = -1;
            measure();
          };

          // ---- density -----------------------------------------------------
          // The app's own stylesheet, swapped under the live document when the
          // density setting flips (ReaderMetrics). Only OUR sheet: the mail's
          // own <style> blocks and inline styles are untouched, and the
          // full-bleed decision above wrote the body's padding inline, so a
          // newsletter that gave its padding up keeps it given up. A different
          // padding or type size is a different width as well as a different
          // height, so the fit is retaken before the measurement.
          window.__passbandRestyle = function (css) {
            var sheet = document.getElementById('passband-sheet');
            if (!sheet || sheet.textContent === css) return;
            sheet.textContent = css;
            fitWidth();
            measure();
          };

          // ---- links -------------------------------------------------------
          // Navigation is refused by the delegate anyway; reporting the href up
          // to native is what makes an in-frame click actually DO something.
          document.addEventListener('click', function (e) {
            var a = e.target && e.target.closest ? e.target.closest('a[href]') : null;
            if (!a) return;
            e.preventDefault();
            var href = a.getAttribute('href') || '';
            if (/^https?:\\/\\//i.test(href)) send({ kind: 'link', value: href });
          }, true);
        })();
        """
}

// MARK: - density inside the frame

/// THE NUMBERS THE APP'S OWN STYLESHEET IS WRITTEN IN, at one density. Web
/// content does not observe anything, so the density reaches a mail frame as a
/// value: the document is built with one of these (`EmailFrameCoordinator.
/// document`), and when the setting flips the coordinator writes the sheet
/// again at the new one and swaps it under the live document. Every field is a
/// COMPACT number run through the curve, so compact is the identity and the
/// sheet is the one that shipped before densities.
///
/// A value rather than a `DensityChoice` because it is what the document is
/// actually drawn in: the pool keys on it, the measurer renders at it, and two
/// densities that happened to agree on all four numbers would draw identical
/// documents.
struct ReaderMetrics: Hashable, Sendable {
    /// The type a body falls back to when it names none of its own.
    var bodyFont: CGFloat
    /// A card document's padding, all four sides.
    var cardPadding: CGFloat
    /// A bubble document's padding: the plain-text bubble's (MessageCard's
    /// `mail`), down and across.
    var bubblePaddingV: CGFloat
    var bubblePaddingH: CGFloat

    /// The sheet as it was written, before there were densities.
    static let compact = ReaderMetrics(
        bodyFont: 14, cardPadding: 14, bubblePaddingV: 9, bubblePaddingH: 12)

    /// The compact size of a body that names no type of its own. A POINT
    /// SMALLER ON THE PHONE: the Mac reads its mail in a wide column beside
    /// 13pt rows, and 14 is the step up a reading column wants. A phone's
    /// rows are 12 and 13 and the column is the screen, where 14 read as the
    /// mail shouting over the app around it.
    ///
    /// `plainBase` is the same thing for a plain-text body, which is drawn as
    /// native text rather than in a web frame.
    #if os(macOS)
        static let bodyBase: CGFloat = 14
        static let plainBase: CGFloat = 13.5
    #else
        static let bodyBase: CGFloat = 13
        static let plainBase: CGFloat = 13
    #endif

    /// At the density in force now.
    @MainActor
    static var current: ReaderMetrics {
        ReaderMetrics(
            bodyFont: Typo.size(bodyBase), cardPadding: Space.s14,
            bubblePaddingV: Space.scaled(9), bubblePaddingH: Space.s12)
    }

    /// The metrics every height in `FrameHeights` was taken at, or nil before
    /// anything has been filed.
    @MainActor private static var filedUnder: ReaderMetrics?

    /// FORGET EVERY HEIGHT TAKEN AT ANOTHER DENSITY. A measured height is the
    /// answer to a question that included the padding and the type, the same
    /// way `FrameHeights.using(width:)` says it included the column, and a
    /// message painted at the other density's height opens wrong and then
    /// snaps. The measurer takes the thread again behind it (ThreadViewer's
    /// `MeasurePass` carries the density).
    ///
    /// Idempotent and cheap, and called from every place that is about to read
    /// or file a height outside a view body: the flip itself reaches several
    /// mounted frames and the reader at once, and whichever asks first clears
    /// the file for all of them. The very first call only records, because the
    /// file is empty: nothing can have been written before somebody asked.
    @MainActor
    static func sync() {
        let now = current
        defer { filedUnder = now }
        guard let was = filedUnder, was != now else { return }
        FrameHeights.shared.wipeAll()
    }
}

// MARK: - the coordinator

/// The owner of a frame while a representable is showing it: load guard, pool
/// bookkeeping, navigation policy. Free-standing rather than nested in a
/// representable, because there are two representables and only one of these.
@MainActor
final class EmailFrameCoordinator: NSObject {
    var onHeight: (FrameMeasurement) -> Void
    /// The navigation this frame started has finished — subresources and all.
    var onLoaded: () -> Void
    var onQuotedFound: (Bool) -> Void
    var onLink: (String) -> Void
    /// What this document holds of the reader's find term. Set on every update
    /// like the four above; not an initializer argument because most hosts
    /// (a preview, a reveal, the forward composer) never ask.
    var onFind: (FindRequest, FindResult) -> Void = { _, _ in }

    /// The frame's permanent hinge, borrowed while this representable owns the
    /// frame. Strong, so `release` can still hand it to the pool after WebKit
    /// has let go of everything else.
    private var relay: FrameRelay?

    /// What is currently loaded, so `updateNSView` only reloads on a real
    /// content/policy change (a reload flashes an empty frame).
    private var loadedSignature: String?
    /// The pool identity of what the frame CURRENTLY HOLDS — re-derived on
    /// every real load, never remembered from checkout: the remote-image
    /// opt-in reloads an already-checked-out frame, so one borrowed under
    /// `allowRemote: false` can go back holding a permissive-CSP document and
    /// must be filed as such.
    private var loadedPoolKey: WebFramePool.Key?
    /// Navigations WE started that have not yet passed the policy gate. A
    /// COUNTER, not a flag: `loadHTMLString` is async, so two loads in quick
    /// succession race for one permission, and with a bool the loser is
    /// cancelled — a permanently blank frame that still reports a height.
    private var pendingOwnLoads = 0

    /// The quoted-history state the HOST wants, and the one the DOCUMENT is
    /// actually in. They are kept apart because `updateNSView` runs for reasons
    /// that have nothing to do with quoted history — a new height, a re-render
    /// of the reader, a fresh set of callback closures — and firing the toggle
    /// each time is a script round trip into every open message's content
    /// process, each one forcing that document to lay itself out again to
    /// answer with a height it already reported. `nil` = the document's state
    /// is unknown, which is what a load in flight leaves behind.
    private var wantedCollapse = true
    private var appliedCollapse: Bool?

    /// THE FIND TERM THE HOST WANTS PAINTED, and the one the document has
    /// painted. Apart for the reason the quoted history's two are: an update
    /// that changed nothing about the find must not cost a script round trip
    /// into every open message. `nil` wanted is no find at all.
    private var wantedFind: FindRequest?
    private var appliedFind: FindRequest?

    /// THE DENSITY THE HOST WANTS THE DOCUMENT DRAWN AT, and the one it is
    /// drawn at. Kept apart for the reason the quoted history's two are: `load`
    /// runs on every update, and a restyle is a script round trip and a full
    /// relayout of the document, worth paying only when the setting moved.
    /// `documentLive` is false while a load is in flight, when the function the
    /// restyle calls does not exist yet; `documentDidLoad` catches up.
    private var wantedMetrics: ReaderMetrics?
    private var appliedMetrics: ReaderMetrics?
    private var documentLive = false
    /// Whether the document is a bubble's or a card's: the stylesheet differs
    /// between the two, so a restyle has to know which one to write. Taken on
    /// every `load`, including the ones the load guard turns away. The guard
    /// covers the bubble flag, so what it turns away is the same document.
    private var documentBubble = false

    init(
        onHeight: @escaping (FrameMeasurement) -> Void, onLoaded: @escaping () -> Void,
        onQuotedFound: @escaping (Bool) -> Void, onLink: @escaping (String) -> Void
    ) {
        self.onHeight = onHeight
        self.onLoaded = onLoaded
        self.onQuotedFound = onQuotedFound
        self.onLink = onLink
    }

    /// The load guard and the pool key must say the same thing about a
    /// document, so both derive from here: drift would make an adopted frame
    /// fail its own guard and reload what it is already showing.
    private static func signature(allowRemote: Bool, document: Int) -> String {
        "\(allowRemote)|\(document)"
    }

    /// The identity of a DOCUMENT, as opposed to a body: the same html is two
    /// documents when one hugs as a bubble and the other fills as a card, and
    /// a frame holding the one must not pass for the other — neither at the
    /// load guard nor in the pool.
    ///
    /// FROM THE PREPARED BODY'S SOURCE HASH, not from the html again: the
    /// prepared html is a pure function of what that hash covers (the raw
    /// body, the tracker policy, the parts), so it identifies the document
    /// exactly as well — and this used to re-hash the whole prepared html on
    /// every `updateNSView`, which is every re-render of the card.
    nonisolated static func documentHash(source: Int, bubble: Bool) -> Int {
        var hasher = Hasher()
        hasher.combine(source)
        hasher.combine(bubble)
        return hasher.finalize()
    }

    func load(
        _ webView: WKWebView, html: String, source: Int, allowRemote: Bool, bubble: Bool,
        poolKey: String?
    ) {
        // NEVER LOAD THE PLACEHOLDER: `prepared` can start empty, and loading
        // a blank document lands the real one as a SECOND navigation — which
        // is what makes the load race reachable at all.
        guard !html.isEmpty else { return }
        // THE DENSITY IS NOT PART OF THE SIGNATURE, on purpose: a flip is a
        // restyle of the document already here, not a new document to load.
        // Reloading every open message to change its padding would throw away
        // the parse, the images and the measured size of each one.
        let metrics = ReaderMetrics.current
        wantedMetrics = metrics
        documentBubble = bubble
        let document = Self.documentHash(source: source, bubble: bubble)
        let signature = Self.signature(allowRemote: allowRemote, document: document)
        guard signature != loadedSignature else {
            applyMetrics(webView, settle: true)
            return
        }
        loadedSignature = signature
        loadedPoolKey = poolKey.map {
            WebFramePool.Key(
                message: $0, allowRemote: allowRemote, document: document, metrics: metrics)
        }
        // The document about to be replaced is the one whose quote state we
        // knew. Until the new one lands there is nothing to toggle — the
        // function the toggle calls does not exist yet — and `documentDidLoad`
        // is what applies the host's wish once it does.
        appliedCollapse = nil
        appliedFind = nil
        // The document about to land is written at these metrics, so there is
        // nothing to restyle when it does, unless the setting moves first.
        appliedMetrics = metrics
        documentLive = false
        relay?.noteLoadStarted()
        pendingOwnLoads += 1
        webView.loadHTMLString(
            Self.document(html: html, allowRemote: allowRemote, bubble: bubble, metrics: metrics),
            // A nil base URL gives the document a unique opaque origin, so
            // relative references resolve to nothing rather than to us.
            baseURL: nil)
    }

    /// Take ownership of a freshly built frame.
    func attach(_ relay: FrameRelay) {
        self.relay = relay
        relay.owner = self
    }

    /// Take ownership of a frame ALREADY showing this document. Re-pointing
    /// `relay.owner` is the entire handover: the message handler and both
    /// delegates are the relay permanently, so nothing on the frame needs
    /// rewiring and no callback can reach the previous coordinator.
    func adopt(_ entry: WebFramePool.Entry, key: WebFramePool.Key) {
        attach(entry.relay)
        // The frame is already loaded, so no load may be started — and
        // `pendingOwnLoads` stays 0, which is correct: layer 4 has no
        // permission outstanding to grant.
        loadedSignature = Self.signature(allowRemote: key.allowRemote, document: key.document)
        loadedPoolKey = key
        // A frame is parked COLLAPSED (WebFramePool.checkIn settles it there),
        // which is also a fresh host's default — so the update that follows
        // this has nothing to toggle and does not say so twice.
        appliedCollapse = true
        // And parked with no find painted (checkIn clears it), so a reader
        // with the bar up paints this one on the update that follows.
        appliedFind = nil
        // A pool hit is a frame parked at the density in force now (the
        // density is in the key), so it is already drawn the way it is wanted.
        appliedMetrics = key.metrics
        wantedMetrics = key.metrics
        documentLive = true

        // A document that was not reloaded never re-runs the measuring script,
        // so a reused frame would sit at zero height behind the paint-hold
        // with no quoted-history chip. The relay's last values release the
        // hold with no round trip; the resend is what makes it right if the
        // frame comes back at a new width. The main-actor hop is not optional
        // — makeNSView runs inside a view update and these callbacks write
        // @State.
        let last = entry.relay.lastMeasurement
        let quoted = entry.relay.lastQuoted
        Task { @MainActor [weak self] in
            guard let self else { return }
            self.onQuotedFound(quoted)
            if let last { self.onHeight(last) }
            // A POOLED FRAME NEVER FIRES `didFinish` AGAIN — it finished
            // loading for its previous host. Saying so here is what leaves the
            // host willing to take one more measurement, which is the whole
            // point of the resend below: the frame may be coming back at a
            // different column width, and the size it was parked at is then the
            // wrong one.
            self.onLoaded()
        }
        entry.webView.evaluateJavaScript("window.__passbandResend && window.__passbandResend()")
    }

    /// Give the frame up: to the pool if it is worth keeping, otherwise to
    /// the teardown path.
    /// Give the frame up to the teardown path unconditionally — for a frame
    /// that must not be parked whatever state its load is in.
    func drop(_ webView: WKWebView) {
        guard let relay else { return }
        relay.owner = nil
        self.relay = nil
        WebFramePool.discard(WebFramePool.Entry(webView: webView, relay: relay))
    }

    func release(_ webView: WKWebView, prerendered: Bool = false) {
        guard let relay else { return }
        relay.owner = nil
        let entry = WebFramePool.Entry(webView: webView, relay: relay)
        self.relay = nil
        // An unfinished or failed load is a trap, not merely worthless: the
        // next owner adopts with no pending-load permission, so the in-flight
        // navigation is refused by layer 4 and the frame comes back blank. A
        // body with no message id has a nil `loadedPoolKey` — that is what
        // keeps sealed reveals out of the pool.
        guard relay.loaded, let key = loadedPoolKey else {
            WebFramePool.discard(entry)
            return
        }
        WebFramePool.shared.checkIn(entry, key: key, prerendered: prerendered)
    }

    func setQuotesCollapsed(_ webView: WKWebView, collapsed: Bool) {
        wantedCollapse = collapsed
        applyCollapse(webView)
    }

    func setFind(_ webView: WKWebView, _ request: FindRequest?) {
        wantedFind = request
        applyFind(webView)
    }

    /// Paint the wanted term and tell the host what the document holds of it.
    /// `fresh` re-reads a document whose DRAWN text changed under a term it
    /// had already searched: the quoted history folding or unfolding.
    private func applyFind(_ webView: WKWebView, fresh: Bool = false) {
        guard documentLive, fresh || appliedFind != wantedFind else { return }
        appliedFind = wantedFind
        guard let request = wantedFind else {
            webView.evaluateJavaScript("window.__passbandFind && window.__passbandFind('')")
            return
        }
        let query = Self.scriptLiteral(request.query)
        webView.evaluateJavaScript(
            "window.__passbandFind ? window.__passbandFind(\(query), \(request.active), \(fresh)) : null"
        ) { [weak self] value, _ in
            // An answer to a question nobody is asking any more: the term or
            // the cursor moved while this one was in the content process.
            guard let self, self.wantedFind == request,
                let result = FindResult(payload: value)
            else { return }
            self.onFind(request, result)
        }
    }

    /// The document just finished loading, so it IS collapsed: the injected
    /// script does that before its first measurement. That is what makes the
    /// state knowable again — and what re-applies an expanded history the
    /// reader had open when something (the remote-image opt-in) reloaded the
    /// frame out from under them.
    ///
    /// It is also the moment the host has been waiting for. WebKit does not
    /// call this until the subresources are in, so the height asked for here
    /// is the one the message came to, images included — and asking is the
    /// point: a pushed height says nothing about which state of the document
    /// produced it, while an answer belongs to the question.
    func documentDidLoad(_ webView: WKWebView) {
        appliedCollapse = true
        documentLive = true
        // The density FIRST, and without a settle of its own: script calls run
        // in the order they are made, so a restyle issued here (the setting
        // moved while this document was loading) is in place before the
        // measurement the settle below asks for.
        applyMetrics(webView, settle: false)
        applyCollapse(webView)
        settle(webView)
        // After the collapse, so the search reads the document as it is drawn.
        applyFind(webView, fresh: true)
    }

    /// A navigation that failed will never report anything, and a host still
    /// waiting for it would refuse every later height this frame ever offers —
    /// including the ones a retry or a resize produces. Releasing the wait
    /// costs nothing when there is genuinely nothing more to come.
    func documentDidFail() {
        onLoaded()
    }

    /// TELL THE HOST TO TAKE ONE LAST MEASUREMENT, and hand it the number.
    ///
    /// Asking is the point: a pushed height says nothing about which state of
    /// the document produced it, while an answer belongs to the question. Used
    /// wherever a document has finished becoming whatever it is about to be —
    /// the navigation landing, and the quoted history folding or unfolding,
    /// which changes the content just as much but starts no navigation at all,
    /// so nothing else would ever tell the host it had stopped moving.
    private func settle(_ webView: WKWebView) {
        onLoaded()
        webView.evaluateJavaScript("window.__passbandMeasure ? window.__passbandMeasure() : 0") {
            [weak self] value, _ in
            guard let self, let m = FrameMeasurement(payload: value) else { return }
            self.onHeight(m)
        }
    }

    /// Swap the app's stylesheet under the live document for the density the
    /// host wants and, when `settle`, tell the host to take the size it came
    /// to, which nothing else would: a restyle starts no navigation.
    private func applyMetrics(_ webView: WKWebView, settle settleAfter: Bool) {
        guard documentLive, let wanted = wantedMetrics, appliedMetrics != wanted else { return }
        appliedMetrics = wanted
        // The pool files a frame under the density it is DRAWN at, and that
        // just changed: parked under the old key, it would be adopted by a
        // reader at the old density and never by one at this.
        loadedPoolKey = loadedPoolKey.map {
            WebFramePool.Key(
                message: $0.message, allowRemote: $0.allowRemote, document: $0.document,
                metrics: wanted)
        }
        let css = Self.scriptLiteral(Self.stylesheet(bubble: documentBubble, metrics: wanted))
        webView.evaluateJavaScript(
            "window.__passbandRestyle && window.__passbandRestyle(\(css))"
        ) { [weak self] _, _ in
            guard settleAfter else { return }
            self?.settle(webView)
        }
    }

    /// A Swift string as a JavaScript string literal. JSON's string grammar is
    /// a subset of JavaScript's, so the encoder's escaping is the right escaping.
    nonisolated private static func scriptLiteral(_ text: String) -> String {
        guard let data = try? JSONEncoder().encode(text),
            let literal = String(data: data, encoding: .utf8)
        else { return "''" }
        return literal
    }

    private func applyCollapse(_ webView: WKWebView) {
        // NOT BEFORE THE DOCUMENT IS HERE. A frame taken from the spares is
        // still showing the empty document it was warmed with until the mail
        // commits, and the settle below would hand the host THAT document's
        // measurement: empty, and as tall as the viewport. The host collapses
        // the card to nothing on the strength of it, the reader lands on a
        // card with no body, and the mail arrives off screen.
        // `documentDidLoad` applies the wish once there is something to fold.
        guard documentLive, appliedCollapse != wantedCollapse else { return }
        appliedCollapse = wantedCollapse
        webView.evaluateJavaScript(
            "window.__passbandSetQuotes && window.__passbandSetQuotes(\(wantedCollapse))"
        ) { [weak self] _, _ in
            // Folding the history is a content change with no navigation
            // behind it, so this is the only thing that ever tells the host
            // the document has finished moving.
            self?.settle(webView)
            // And what is drawn is what a find matches, so the term in force
            // is searched again: unfolding the history is how its matches
            // come into the count at all.
            self?.applyFind(webView, fresh: true)
        }
    }

    /// LAYER 4: allow exactly the in-memory loads WE started, refuse everything
    /// else — a link click cannot navigate this view anywhere. Called by the
    /// relay; the counter lives here because only the owner starts loads.
    func decideNavigation(_ navigationAction: WKNavigationAction) -> WKNavigationActionPolicy {
        if pendingOwnLoads > 0 {
            pendingOwnLoads -= 1
            return .allow
        }
        // A user-initiated link becomes a system-browser open (Opener
        // re-guards to http/https); anything else is silently dropped.
        if navigationAction.navigationType == .linkActivated,
            let url = navigationAction.request.url
        {
            onLink(url.absoluteString)
        }
        return .cancel
    }

    /// Build the full document. The CSP meta MUST be the first thing in <head>
    /// so it governs every subsequent resource. The reading surface is
    /// deliberately OPAQUE white: body copy over a live wallpaper is
    /// unreadable, and mail ships its own colors assuming a white canvas.
    ///
    /// THE HEIGHT RULE IS LOAD-BEARING, and it is not cosmetic. This frame is
    /// sized to what the document measures, so a document whose own height is a
    /// function of the viewport is a closed loop with gain 1: it reports the
    /// viewport's height, the frame becomes that, the viewport grows by the
    /// padding above, it reports again. Measured at exactly +28 points a pass,
    /// about eighty passes a second, without bound — a message that grows until
    /// you leave the thread, on the same main thread that moves the scroll.
    ///
    /// `html,body{height:100%}` is how a great many email templates open, and
    /// our stylesheet used to set no height at all, so theirs applied
    /// unopposed. `!important` beats an ordinary author rule wherever it sits.
    /// Verified across documents up to 16,842 points not to move an ordinary
    /// body by a single point.
    ///
    /// `pre` wraps because the body is `overflow:hidden`: a plain-text message
    /// sent as one `<pre>` (Google Cloud Support does this) otherwise runs a
    /// paragraph off the right edge and it is simply gone. `overflow-wrap`
    /// alone does nothing there, since `white-space:pre` never offers a break.
    /// `!important` because the same senders also write it inline.
    ///
    /// It does NOT cover `vh` units, which resolve against the viewport no
    /// matter what html and body are told to be. Those are bounded on the host
    /// side instead — see EmailWebView's sizing phases.
    nonisolated static func document(
        html: String, allowRemote: Bool, bubble: Bool = false, metrics: ReaderMetrics
    ) -> String {
        // Which schemes an image may come from, and why the two custom ones are
        // gated differently, is MailCSP's — the policy lives where it can be
        // asserted without a WebKit process.
        let csp = MailCSP.policy(allowRemote: allowRemote)
        // A PHONE lays a viewport-less document out at 980 CSS px and then
        // scales the result down to fit, which renders every email at about a
        // third size. Pinning the layout viewport to the frame's own width is
        // also what makes the height the bridge reports arrive in units the
        // outer SwiftUI ScrollView can use as points. A Mac frame IS its layout
        // width already, so the tag would be a no-op there and is left out.
        #if os(macOS)
            let viewport = ""
        #else
            let viewport = "<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">"
        #endif
        return """
            <!doctype html><html><head>\
            <meta http-equiv="Content-Security-Policy" content="\(csp)">\
            <meta name="referrer" content="no-referrer">\
            <meta charset="utf-8">\(viewport)\
            <style id="passband-sheet">\(stylesheet(bubble: bubble, metrics: metrics))</style>\
            </head><body\(bubble ? " class=\"hug\"" : "")>\(html)</body></html>
            """
    }

    /// THE APP'S OWN STYLESHEET, the one `<style>` in the document that is
    /// ours, and the only thing the density reaches inside a mail frame.
    ///
    /// WHAT FOLLOWS THE DENSITY IS WHAT WE DREW: the padding between our frame
    /// and the mail, and the type a body falls back to when it names none of
    /// its own (a reply typed in Gmail, any message that is plain prose), so an
    /// html reply reads at the size the plain-text one beside it does. WHAT
    /// DOES NOT is anything that shapes the SENDER'S markup: the quote rule, the
    /// link colour, the image and table caps. Those are defaults under the
    /// mail, and the mail looks the way its author made it. A body that sets
    /// its own type is untouched either way: its rules come after this sheet
    /// and win.
    ///
    /// Swapped whole under a live document by `__passbandRestyle` when the
    /// setting flips, so it is a function of its inputs and nothing else, and
    /// in compact it is byte for byte the sheet that shipped before densities.
    nonisolated static func stylesheet(bubble: Bool, metrics: ReaderMetrics) -> String {
        "html,body{margin:0;padding:0;background:#fff;color:#111;"
            + "font:\(px(metrics.bodyFont))/1.55 -apple-system,BlinkMacSystemFont,'SF Pro Text',sans-serif;"
            + "word-break:break-word;overflow-wrap:anywhere;overflow:hidden;}"
            + "html,body{height:auto!important;min-height:0!important;}"
            + (bubble ? bubbleBody(metrics) : cardBody(metrics))
            + "img{max-width:100%;height:auto;}"
            + "a{color:#2b7fd4;}"
            + "table{max-width:100%;}"
            + "pre{white-space:pre-wrap!important;}"
            + "blockquote{margin:8px 0;padding-left:10px;border-left:2px solid #d8dee6;color:#555;}"
    }

    /// A length in CSS pixels, which are points here. Whole numbers print as
    /// they always did; the comfortable curve snaps to the half point.
    nonisolated private static func px(_ value: CGFloat) -> String {
        value == value.rounded() ? "\(Int(value))px" : "\(Double(value))px"
    }

    /// THE BODY IS THE ONLY PADDED BOX. It used to be `html,body{padding:14px}`,
    /// which pads both and puts the text 28 points in — a slab, in a bubble.
    /// The measuring script reads the body's box, so what it reports is what
    /// is drawn.
    nonisolated private static func cardBody(_ metrics: ReaderMetrics) -> String {
        "body{padding:\(px(metrics.cardPadding));}"
    }
    /// A bubble is padded like the plain-text bubble beside it (PlainBody: 12
    /// across, 9 down) and is only as wide as what is in it: `fit-content` is
    /// min(the widest line, the frame), border-box so the cap includes the
    /// padding. A one-line reply hugs; a newsletter's 600px table fills.
    ///
    /// THE HUG IS A CLASS, so the measuring script can take it away: a body
    /// built from `width="100%"` tables and percentage-width images answers
    /// the question "how wide do you want to be" with nothing (percentages
    /// contribute zero to an intrinsic width), and a frame narrowed to that
    /// answer caps the document at it for good. Such a document fills.
    nonisolated private static func bubbleBody(_ metrics: ReaderMetrics) -> String {
        "body{padding:\(px(metrics.bubblePaddingV)) \(px(metrics.bubblePaddingH));box-sizing:border-box;}"
            + "body.hug{width:-webkit-fit-content;width:fit-content;max-width:100%;}"
    }
}

/// THE LINE AHEAD OF THE DOCUMENT. A sanitized body very often opens with
/// bare text and then a `<style>`: that is the head of the original mail with
/// its tags removed — the `<title>` text survives, the `<style>` block after it
/// survives — and rendered as-is it is a stray first line on the page (in this
/// mailbox, the top of some 990 messages). It IS the mail's topline, so it is
/// shown, as a caption over the body rather than inside it.
///
/// Only that exact shape is taken: text, then `<style`. A message that opens
/// "Hello,<br>" opens with text too, and that text is the mail.
enum Topline {
    static func split(_ html: String) -> (topline: String?, html: String) {
        let trimmedStart = html.drop(while: { $0.isWhitespace })
        guard let lt = trimmedStart.firstIndex(of: "<"), lt != trimmedStart.startIndex,
            trimmedStart[lt...].prefix(6).lowercased() == "<style"
        else { return (nil, html) }
        let raw = trimmedStart[trimmedStart.startIndex..<lt]
        let text = decode(String(raw))
            .split(whereSeparator: \.isWhitespace).joined(separator: " ")
        guard !text.isEmpty, text.count <= 300 else { return (nil, html) }
        return (text, String(trimmedStart[lt...]))
    }

    /// Entities, decoded UNTIL THEY STOP DECODING: the sanitizer escaped what
    /// was already escaped, so the stored text reads `&amp;#x27;` where the
    /// mail said an apostrophe. Two passes is the common case; the loop is
    /// bounded either way.
    static func decode(_ text: String) -> String {
        var out = text
        for _ in 0..<3 {
            let next = decodeOnce(out)
            if next == out { return out }
            out = next
        }
        return out
    }

    private static let named: [String: String] = [
        "amp": "&", "lt": "<", "gt": ">", "quot": "\"", "apos": "'", "nbsp": " ",
        "mdash": "\u{2014}", "ndash": "\u{2013}", "hellip": "\u{2026}", "copy": "\u{A9}",
        "reg": "\u{AE}", "trade": "\u{2122}", "lsquo": "\u{2018}", "rsquo": "\u{2019}",
        "ldquo": "\u{201C}", "rdquo": "\u{201D}", "bull": "\u{2022}", "zwnj": "", "zwj": "",
        "shy": "", "euro": "\u{20AC}", "pound": "\u{A3}", "yen": "\u{A5}", "deg": "\u{B0}",
        "times": "\u{D7}", "laquo": "\u{AB}", "raquo": "\u{BB}", "rarr": "\u{2192}",
        "larr": "\u{2190}", "middot": "\u{B7}", "eacute": "\u{E9}", "egrave": "\u{E8}",
        "ecirc": "\u{EA}", "aacute": "\u{E1}", "agrave": "\u{E0}", "acirc": "\u{E2}",
        "auml": "\u{E4}", "iacute": "\u{ED}", "oacute": "\u{F3}", "ouml": "\u{F6}",
        "uacute": "\u{FA}", "uuml": "\u{FC}", "ntilde": "\u{F1}", "ccedil": "\u{E7}",
        "Eacute": "\u{C9}", "Uuml": "\u{DC}", "Ouml": "\u{D6}", "Auml": "\u{C4}",
    ]

    private static func decodeOnce(_ text: String) -> String {
        guard text.contains("&") else { return text }
        var out = ""
        var rest = Substring(text)
        while let amp = rest.firstIndex(of: "&") {
            out += rest[rest.startIndex..<amp]
            let tail = rest[amp...]
            guard let semi = tail.firstIndex(of: ";"), tail.distance(from: amp, to: semi) <= 10
            else {
                out.append("&")
                rest = tail.dropFirst()
                continue
            }
            let name = tail[tail.index(after: amp)..<semi]
            var replacement: String?
            if name.hasPrefix("#x") || name.hasPrefix("#X") {
                replacement = UInt32(name.dropFirst(2), radix: 16)
                    .flatMap(Unicode.Scalar.init).map { String(Character($0)) }
            } else if name.hasPrefix("#") {
                replacement = UInt32(name.dropFirst())
                    .flatMap(Unicode.Scalar.init).map { String(Character($0)) }
            } else {
                replacement = named[String(name)]
            }
            if let replacement {
                out += replacement
                rest = tail[tail.index(after: semi)...]
            } else {
                out.append("&")
                rest = tail.dropFirst()
            }
        }
        out += rest
        return out
    }
}

/// What the measuring script says about the document it is in — the height
/// the frame should be, the width the body came to, and whether there is
/// anything in it at all. Pushed on every change and pulled on demand
/// (`__passbandMeasure`), the same shape either way.
struct FrameMeasurement: Equatable, Sendable {
    var height: CGFloat
    var width: CGFloat
    var empty: Bool
    /// The page's computed background, or nil when it was not a plain colour
    /// (a gradient, an image, nothing reported). The tail is drawn in it.
    var background: Color?

    /// From a script message or an `evaluateJavaScript` result — a dictionary
    /// with NSNumber values either way. nil for anything that is not a
    /// measurement, including a height of zero (a document that did not run
    /// the script).
    init?(payload: Any?) {
        guard let dict = payload as? [String: Any],
            let height = (dict["height"] as? NSNumber)?.doubleValue ?? (dict["value"] as? NSNumber)?.doubleValue,
            height > 0
        else { return nil }
        self.height = CGFloat(height)
        self.width = CGFloat((dict["width"] as? NSNumber)?.doubleValue ?? 0)
        self.empty = (dict["empty"] as? Bool) ?? false
        self.background = Self.color(css: dict["background"] as? String)
    }

    /// `rgb(r, g, b)` / `rgba(r, g, b, a)` as `getComputedStyle` spells a
    /// colour, to a Color. Anything else — transparent, a keyword, nothing —
    /// is nil, and the caller keeps what it had.
    static func color(css: String?) -> Color? {
        guard let css, let open = css.firstIndex(of: "("), let close = css.lastIndex(of: ")"),
            css.hasPrefix("rgb")
        else { return nil }
        let parts = css[css.index(after: open)..<close]
            .split(whereSeparator: { $0 == "," || $0 == " " || $0 == "/" })
            .compactMap { Double($0) }
        guard parts.count >= 3 else { return nil }
        if parts.count >= 4, parts[3] == 0 { return nil }
        return Color(
            .sRGB, red: parts[0] / 255, green: parts[1] / 255, blue: parts[2] / 255, opacity: 1)
    }
}

// MARK: - the live frame pool

/// A frame's ONE permanent connection to native code: script message handler,
/// navigation delegate and UI delegate, installed at construction and never
/// replaced. A pooled frame outlives its representable, and one
/// `WKUserContentController` will not hold two handlers under a single name, so
/// re-`add`ing each new Coordinator would either throw or pin a torn-down view's
/// closures to the frame — instead the wiring never changes and OWNERSHIP does,
/// in one assignment, nil while the frame sits in the pool. Layer 4 is anchored
/// here: an unowned frame refuses every navigation. Height/quoted keep being
/// recorded with no owner, so a checked-in frame still leaves the pool a truthful
/// measurement for the next open.
@MainActor
final class FrameRelay: NSObject, WKScriptMessageHandler, WKNavigationDelegate,
    WKUIDelegate
{
    static let name = "passband"

    /// Weak on purpose: SwiftUI owns coordinators, and a frame must never be
    /// the reason a dead view's callbacks stay alive.
    weak var owner: EmailFrameCoordinator?

    /// What THIS document last told us. Read at checkout, because the measuring
    /// script does not re-run for a frame that was not reloaded.
    private(set) var lastMeasurement: FrameMeasurement?
    private(set) var lastQuoted = false
    /// Whether the current document finished loading. Only a settled frame is safe
    /// to pool — see Coordinator.release.
    private(set) var loaded = false

    func noteLoadStarted() {
        loaded = false
        lastMeasurement = nil
        lastQuoted = false
    }

    func userContentController(
        _ controller: WKUserContentController, didReceive message: WKScriptMessage
    ) {
        guard let payload = message.body as? [String: Any],
            let kind = payload["kind"] as? String
        else { return }
        switch kind {
        case "height":
            guard let m = FrameMeasurement(payload: payload) else { return }
            lastMeasurement = m
            owner?.onHeight(m)
        case "quoted":
            guard let value = payload["value"] as? Bool else { return }
            lastQuoted = value
            owner?.onQuotedFound(value)
        // Dropped with no owner: a pooled frame is off screen and cannot be
        // clicked, so such a message could only be stale.
        case "link":
            if let value = payload["value"] as? String { owner?.onLink(value) }
        default: break
        }
    }

    /// The empty document a SPARE is warmed with, which is loaded before this
    /// relay is anybody's delegate and may still be at the policy gate when the
    /// spare is handed out. It gets its own claim so that it can never spend
    /// the COORDINATOR's: both are "a load we started", they are told apart by
    /// who started them, and a warming load that consumed the real one's
    /// permission would leave the message that took the spare permanently
    /// blank.
    private var pendingWarmLoads = 0
    func expectWarmingLoad() { pendingWarmLoads += 1 }

    /// LAYER 4 — the policy itself is Coordinator.decideNavigation. NOTE the exact
    /// signature: the closure must be `@MainActor @Sendable` or Swift treats this
    /// as an unrelated near-miss method, the delegate requirement goes
    /// unimplemented, and WebKit silently defaults to ALLOWING every navigation.
    func webView(
        _ webView: WKWebView, decidePolicyFor navigationAction: WKNavigationAction,
        decisionHandler: @escaping @MainActor @Sendable (WKNavigationActionPolicy) -> Void
    ) {
        if pendingWarmLoads > 0 {
            pendingWarmLoads -= 1
            decisionHandler(.allow)
            return
        }
        // No owner means the frame is parked in the pool: nobody is entitled to
        // navigate it, so nothing may.
        guard let owner else {
            decisionHandler(.cancel)
            return
        }
        decisionHandler(owner.decideNavigation(navigationAction))
    }

    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
        loaded = true
        // The new document is collapsed and toggleable again; only the owner
        // knows which state its host is asking for.
        owner?.documentDidLoad(webView)
    }

    func webView(
        _ webView: WKWebView, didFail navigation: WKNavigation!, withError error: Error
    ) {
        loaded = false
        owner?.documentDidFail()
    }

    func webView(
        _ webView: WKWebView, didFailProvisionalNavigation navigation: WKNavigation!,
        withError error: Error
    ) {
        loaded = false
        owner?.documentDidFail()
    }

    /// No popups, ever.
    func webView(
        _ webView: WKWebView, createWebViewWith configuration: WKWebViewConfiguration,
        for navigationAction: WKNavigationAction, windowFeatures: WKWindowFeatures
    ) -> WKWebView? {
        if let url = navigationAction.request.url { owner?.onLink(url.absoluteString) }
        return nil
    }
}

/// LRU pool of LIVE, ALREADY-RENDERED email frames: a reopened message skips the
/// content-process attach, parse, layout, measuring pass and image reattachment.
/// FrameHeights makes it the right size; this makes it already drawn.
@MainActor
final class WebFramePool {
    static let shared = WebFramePool()

    /// SMALL ON PURPOSE: each entry is a live WebKit content-process attachment
    /// plus a whole email's retained layout tree. Six covers walking a thread back
    /// and forth and flipping between the last few messages; past that the parse
    /// being avoided is cheaper than the residency being paid for.
    private static let capacity = 6

    /// The identity of a RENDERED DOCUMENT, not of a message: all three fields
    /// must match, so a hit is showing a BYTE-IDENTICAL document under an
    /// identical CSP. Dropping any one weakens the sandbox (see docs/SECURITY.md)
    /// — `allowRemote` in particular IS the CSP, so the remote-image opt-in must
    /// be a pool MISS. Quote collapse is deliberately NOT in the key: it is a JS
    /// call on the live document, and frames are checked in collapsed so a hit
    /// matches a fresh open's default state.
    ///
    /// THE DENSITY IS IN IT TOO, and unlike the other three it is not about the
    /// sandbox: a frame parked at the other density is drawn with the other
    /// padding and type, and its last measurement (what an adopting view shows
    /// first) is that drawing's height. A live frame is restyled in place when
    /// the setting flips and re-filed under the new key; a parked one is simply
    /// a miss until it ages out.
    struct Key: Hashable {
        let message: String
        let allowRemote: Bool
        let document: Int
        let metrics: ReaderMetrics
    }

    /// The frame is held as the BASE class: the Mac subclasses WKWebView to give
    /// the scroll wheel back to the column that owns the scroll, and a phone has
    /// nothing to override, so the pool must not care which it is holding.
    struct Entry {
        let webView: WKWebView
        let relay: FrameRelay
    }

    private var frames: [Key: Entry] = [:]
    /// Least-recently-used first.
    private var order: [Key] = []
    /// THE SECOND TIER: frames rendered AHEAD of a click (FramePrewarmer),
    /// for mail nobody has opened yet. Kept apart from the read frames so
    /// that pre-rendering the board's top rows can never evict the message
    /// somebody just walked away from, and capped on its own — each is a
    /// live content process, and the board only ever shows a few rows a
    /// click away.
    private static let prerenderedCapacity = 4
    private var prerendered: Set<Key> = []

    private init() {}

    /// Whether a frame for this document is parked, in either tier.
    func holds(_ key: Key) -> Bool { frames[key] != nil }

    /// A parked frame holding exactly this document, or nil. Checkout REMOVES it,
    /// which is the whole answer to a frame being in use: two views of one message
    /// can never be handed the same WKWebView — the second builds fresh, and
    /// whichever is torn down last is the one that ends up parked.
    func checkout(_ key: Key) -> Entry? {
        guard let entry = frames.removeValue(forKey: key) else { return nil }
        order.removeAll { $0 == key }
        prerendered.remove(key)
        return entry
    }

    func checkIn(_ entry: Entry, key: Key, prerendered: Bool = false) {
        entry.relay.owner = nil
        entry.webView.removeFromSuperview()
        // Settle back to the state a fresh open expects: a frame parked with its
        // quoted history expanded would be re-shown tall and then snap shorter
        // under the new view's collapsed default. The relay records the resulting
        // height with no owner attached, so it is what the next checkout reports.
        entry.webView.evaluateJavaScript(
            "window.__passbandSetQuotes && window.__passbandSetQuotes(true)")
        // And with nothing highlighted: the paint lives in the document, so a
        // frame parked mid-find would come back still lit for a term nobody
        // is looking for.
        entry.webView.evaluateJavaScript("window.__passbandFind && window.__passbandFind('')")

        // Two frames of one document: keep the one just used.
        if let incumbent = frames.removeValue(forKey: key) {
            order.removeAll { $0 == key }
            self.prerendered.remove(key)
            Self.discard(incumbent)
        }
        frames[key] = entry
        order.append(key)
        if prerendered { self.prerendered.insert(key) }

        // Each tier evicts its own oldest, and neither counts the other.
        evict(tier: prerendered, capacity: prerendered ? Self.prerenderedCapacity : Self.capacity)
    }

    private func evict(tier: Bool, capacity: Int) {
        while order.filter({ prerendered.contains($0) == tier }).count > capacity {
            guard let oldest = order.first(where: { prerendered.contains($0) == tier }) else { return }
            order.removeAll { $0 == oldest }
            prerendered.remove(oldest)
            if let evicted = frames.removeValue(forKey: oldest) { Self.discard(evicted) }
        }
    }

    /// Drop every parked frame. An account switch: the key's `message` is a
    /// message id, which is one daemon's — and a live WebKit attachment still
    /// holding the old account's document is not something to hand the new one
    /// on a key collision.
    func wipeAll() {
        for (_, entry) in frames { Self.discard(entry) }
        frames.removeAll()
        order.removeAll()
        prerendered.removeAll()
        for spare in spares { Self.discard(spare) }
        spares.removeAll()
    }

    // MARK: - warm spares

    /// BLANK FRAMES, ALREADY WARM. A pool hit is a message you have already
    /// read; this is for the ones you have not.
    ///
    /// Building a WKWebView is cheap (about three milliseconds of main thread).
    /// What is not cheap is what happens after: a brand new frame takes some
    /// seventy milliseconds to put anything on screen, because its content
    /// process is starting up. That time is not a freeze — the main thread
    /// stays responsive throughout — it is a BLANK BOX where a message should
    /// be, and on a first scroll through a thread there is one per message.
    ///
    /// A frame that has already had a document through it renders the next one
    /// in about five. So spares are built ahead of the scroll, put through an
    /// empty document to wake their process, and handed out the moment a
    /// message needs one. Construction alone warms nothing — measured: a spare
    /// built and never loaded still pays the full seventy — so the empty load
    /// IS the warming, and it holds: a spare that has sat idle for ninety
    /// seconds is still within a millisecond or two of a hot one.
    private static let spareTarget = 2
    private var spares: [Entry] = []
    private var refilling = false

    /// ONE warm frame in the drawer, at launch, so the first message of the
    /// session is not the one that waits for a content process. Does nothing
    /// once there is anything to hand out.
    func seedSpare(_ build: @MainActor () -> Entry) {
        guard spares.isEmpty else { return }
        spares.append(build())
    }

    /// A warm blank frame, or nil if none is ready. The caller wires its own
    /// delegates: a spare is deliberately handed out UNWIRED (see the builders
    /// in the two representables) — layer 4 refuses every navigation a frame's
    /// relay does not own, which would include the empty one that warms it.
    func takeSpare() -> Entry? {
        spares.popLast()
    }

    /// Top the spares back up, NEXT TURN — never inside the mount that just
    /// took one. The build is only a few milliseconds, but they are the same
    /// few milliseconds the scroll is trying to use.
    func replenishSpares(_ build: @escaping @MainActor () -> Entry) {
        guard !refilling, spares.count < Self.spareTarget else { return }
        refilling = true
        Task { @MainActor [weak self] in
            guard let self else { return }
            defer { self.refilling = false }
            while self.spares.count < Self.spareTarget {
                self.spares.append(build())
                // One per turn: two content processes starting at once, during
                // a scroll, is the thing being avoided.
                await Task.yield()
            }
        }
    }

    /// A dropped frame has to be UNWIRED, not just released: the content controller
    /// retains the relay and the relay is the frame's delegate, so letting go
    /// leaves a live target for a late navigation or script callback.
    static func discard(_ entry: Entry) {
        entry.relay.owner = nil
        entry.webView.stopLoading()
        entry.webView.navigationDelegate = nil
        entry.webView.uiDelegate = nil
        let controller = entry.webView.configuration.userContentController
        controller.removeScriptMessageHandler(forName: FrameRelay.name)
        controller.removeAllUserScripts()
        entry.webView.removeFromSuperview()
    }
}


/// THE PREPARED-BODY KEY, REMEMBERED PER MESSAGE. `Prepared.cacheKey` hashes
/// the whole html, and it was being asked in `EmailWebView.init` and again
/// from `body` on every evaluation of every card. Keyed on the message id
/// and the body's length: a healed body (the blank-body heal replaces the html
/// under the same id) is a different length and misses, and nothing else ever
/// changes a message's html under its id. A body with no id is hashed
/// outright — those are sealed reveals and the forward composer's preview,
/// one at a time.
///
/// Lock-based for the same reason PreparedBodies is: `EmailWebView.init`
/// runs nonisolated.
final class PreparedKeyMemo: @unchecked Sendable {
    static let shared = PreparedKeyMemo()
    private struct Identity: Hashable {
        let message: String
        let length: Int
        let allowTrackers: Bool
        let parts: Int
    }
    private let lock = NSLock()
    private var entries = LRUMap<Identity, Int>(limit: 600)
    private init() {}

    /// Account switch: the keys are message ids, one daemon's. A survivor
    /// would hand another account's message the first one's prepared body
    /// on an id-and-length collision.
    func wipe() {
        lock.lock()
        defer { lock.unlock() }
        entries = LRUMap<Identity, Int>(limit: 600)
    }

    func key(message: String?, html: String, allowTrackers: Bool, attachments: [Attachment]) -> Int {
        guard let message else {
            return EmailWebView.Prepared.cacheKey(html, allowTrackers, attachments)
        }
        var parts = Hasher()
        for att in attachments { parts.combine(att.id) }
        let identity = Identity(
            message: message, length: html.utf8.count, allowTrackers: allowTrackers,
            parts: parts.finalize())
        lock.lock()
        defer { lock.unlock() }
        if let known = entries.get(identity) { return known }
        let made = EmailWebView.Prepared.cacheKey(html, allowTrackers, attachments)
        entries.set(identity, made)
        return made
    }
}

// MARK: - rendering ahead of the click

/// FRAMES RENDERED BEFORE ANYBODY CLICKS. The thread prefetch makes an open
/// network-free and the body warmer makes it scan-free; what was left was the
/// web frame itself — a spare's content process is warm, but the document
/// still has to be parsed, laid out, measured and its images fetched after
/// the click, which is a blank box for the first hundred-odd milliseconds of
/// every unread email. A pool hit skips all of it, and until now only a
/// message somebody had ALREADY read could be one.
///
/// This renders the NEWEST message of each wanted thread — the one the reader
/// opens on — into a frame parked in the pool's prerendered tier, under the
/// exact key the reader's mount will ask for: same style resolution, same
/// tracker policy, same image policy (the CSP), same bubble-or-card document.
/// Any of those differing is a miss, never a wrong frame.
///
/// ONE AT A TIME, off the interaction path: each render is a content-process
/// load, and two starting at once during a scroll is the thing spares exist
/// to avoid. The queue is replaced wholesale by `want` — the board's top rows
/// are the only ones worth a frame, and they change.
@MainActor
final class FramePrewarmer {
    static let shared = FramePrewarmer()

    private var queue: [String] = []
    /// THE OPEN THREAD'S OWN NEIGHBOURS: the messages just past either edge of
    /// the window, which the reader is about to scroll onto. Served before the
    /// board's queue, because these are a scroll away rather than a click.
    private struct Neighbour {
        let message: ClientMessage
        let style: ThreadStyle
        let width: CGFloat
    }
    private var neighbours: [Neighbour] = []
    private var running = false
    private var inflight: Set<WebFramePool.Key> = []
    /// Bumped by `wipe`: an account switch must abandon the queue AND refuse
    /// to park anything a render started under the old account.
    private var generation = 0
    /// The viewport a pre-rendered document is laid out in. Only `vh` units
    /// care, and an adopted frame re-reports at its real size anyway.
    private static let viewport: CGFloat = 800
    /// Longer than the measurer's load deadline: images are part of what is
    /// being rendered ahead, and a frame that is not finished is not parked.
    private static let loadDeadline: Duration = .seconds(4)

    private init() {}

    /// The threads worth a frame, in priority order. Replaces the queue.
    func want(_ threadIds: [String]) {
        var seen = Set<String>()
        queue = threadIds.filter { !$0.isEmpty && seen.insert($0).inserted }
        kick()
    }

    /// The messages the open reader is about to scroll onto, nearest first.
    /// Replaces the last answer: the window has moved, and what was next to
    /// it a moment ago is either mounted now or behind it.
    ///
    /// A message mounting in a LAZY stack is a frame built at the moment it
    /// comes into view: parse, layout and measure all happen with its space
    /// already on screen, which is a blank box and then mail flashing into
    /// it, once per message, all the way up a long thread. Rendered here
    /// first, the mount is a pool hit and the message is simply there.
    func ahead(_ messages: [ClientMessage], style: ThreadStyle, width: CGFloat) {
        guard width > 0 else { return }
        neighbours = messages.map { Neighbour(message: $0, style: style, width: width) }
        kick()
    }

    /// Account switch: nothing queued belongs to the new daemon.
    func wipe() {
        generation &+= 1
        queue.removeAll()
        neighbours.removeAll()
        nudged?.cancel()
        nudged = nil
    }

    /// The row under the pointer: rendered ahead of the others, but only once
    /// the pointer has RESTED there — crossing ten rows on the way to one
    /// must not queue ten documents.
    private var nudged: Task<Void, Never>?
    func nudge(_ threadId: String) {
        nudged?.cancel()
        nudged = Task { @MainActor [weak self] in
            try? await Task.sleep(for: .milliseconds(150))
            guard let self, !Task.isCancelled else { return }
            self.queue.removeAll { $0 == threadId }
            self.queue.insert(threadId, at: 0)
            self.kick()
        }
    }

    /// Render these next, ahead of whatever the board wants, without
    /// replacing its queue: a lesson about to ask for a click on them.
    func prefer(_ threadIds: [String]) {
        let ids = threadIds.filter { !$0.isEmpty }
        queue.removeAll { ids.contains($0) }
        queue.insert(contentsOf: ids, at: 0)
        kick()
    }

    private func kick() {
        guard !running else { return }
        running = true
        let gen = generation
        Task { @MainActor [weak self] in
            defer { self?.running = false }
            while let self, gen == self.generation,
                !(self.queue.isEmpty && self.neighbours.isEmpty)
            {
                if !self.neighbours.isEmpty {
                    let next = self.neighbours.removeFirst()
                    // The read tier, not the prerendered one: this is the
                    // open thread's mail, and it must not push the board's
                    // rows out of theirs.
                    await self.render(
                        next.message, style: next.style, width: next.width, prerendered: false,
                        gen: gen)
                } else {
                    let next = self.queue.removeFirst()
                    await self.render(next, gen: gen)
                }
                // A beat between renders, so a click landing mid-queue is
                // served before the next document starts.
                await Task.yield()
            }
        }
    }

    private func render(_ threadId: String, gen: Int) async {
        let store = AppStore.shared
        // NOT DURING A FLIGHT. The board's list changes the moment done+next
        // resolves a row, and that used to start a content-process load in
        // the middle of the very animation this is meant to keep smooth.
        // Wait it out, and a beat more so a click landing right after a list
        // change is served before the next document starts.
        while store.threadFlight != .settled {
            try? await Task.sleep(for: .milliseconds(100))
            guard gen == generation else { return }
        }
        try? await Task.sleep(for: .milliseconds(250))
        guard gen == generation else { return }
        // The open thread's newest message is mounted — its frame is checked
        // out, so the pool cannot see it, and rendering a second copy would
        // only be discarded when the first comes back.
        guard store.threadId != threadId else { return }
        guard let view = try? await ThreadPrefetch.shared.fetch(threadId),
            gen == generation, view.cache_allowed == true,
            let message = view.messages.last, let html = message.html, !html.isEmpty
        else { return }
        let style = ThreadViewer.resolveStyle(
            threadId: threadId, messages: view.messages,
            readingMail: ThreadViewer.isReadingMail(threadId, store))
        let width = ThreadViewer.expectedBodyWidth(style: style)
        guard width > 0 else { return }
        await render(message, style: style, width: width, prerendered: true, gen: gen)
    }

    /// One message into one parked frame, under the key its card will ask for.
    private func render(
        _ message: ClientMessage, style: ThreadStyle, width: CGFloat, prerendered: Bool, gen: Int
    ) async {
        guard let html = message.html, !html.isEmpty else { return }
        let bubble = style == .bubbles
        let allowRemote = Prefs.shared.loadRemoteImages

        // The prepared body, from the warmer if it has been here, made here
        // (off the main actor) if not.
        let allow = message.allowsTrackers
        let parts = message.attachmentList
        let key = EmailWebView.Prepared.cacheKey(html, allow, parts)
        let prepared: EmailWebView.Prepared
        if let warm = PreparedBodies.shared.get(key) {
            prepared = warm
        } else {
            prepared = await Task.detached(priority: .utility) {
                EmailWebView.Prepared.make(from: html, allowTrackers: allow, attachments: parts)
            }.value
            PreparedBodies.shared.set(key, prepared)
        }
        guard gen == generation, !prepared.html.isEmpty else { return }

        let poolKey = style.frameKey(message.id)
        guard
            let renderKey = EmailFrame.renderKey(
                poolKey: poolKey, html: prepared.html, source: prepared.sourceHash,
                allowRemote: allowRemote, bubble: bubble)
        else { return }
        guard !WebFramePool.shared.holds(renderKey), !inflight.contains(renderKey) else { return }
        inflight.insert(renderKey)
        defer { inflight.remove(renderKey) }

        let entry = WebFramePool.shared.takeSpare() ?? EmailWebViewRepresentable.buildFrame()
        WebFramePool.shared.replenishSpares(EmailWebViewRepresentable.buildSpare)
        let webView = entry.webView
        webView.navigationDelegate = entry.relay
        webView.uiDelegate = entry.relay
        // NO WINDOW AND NO SUPERVIEW, like the measurer's frame: a web view
        // held in a variable parses, lays out and runs the injected script
        // exactly as one in a window does.
        webView.frame = CGRect(x: 0, y: 0, width: width, height: Self.viewport)

        let coordinator = EmailFrameCoordinator(
            onHeight: { _ in }, onLoaded: {}, onQuotedFound: { _ in }, onLink: { _ in })
        let landed = await withCheckedContinuation { (continuation: CheckedContinuation<Bool, Never>) in
            let once = Once()
            coordinator.onLoaded = { once.run { continuation.resume(returning: true) } }
            coordinator.attach(entry.relay)
            coordinator.load(
                webView, html: prepared.html, source: prepared.sourceHash,
                allowRemote: allowRemote, bubble: bubble, poolKey: poolKey)
            Task { @MainActor in
                try? await Task.sleep(for: Self.loadDeadline)
                once.run { continuation.resume(returning: false) }
            }
        }
        // A frame that did not land in time, or landed for an account that
        // has since been switched away, is DROPPED, never parked: `release`
        // would park it if the navigation happened to finish between the
        // deadline firing and this task resuming, and a parked frame that
        // is then gutted is a blank card on the next click. `onLoaded` also
        // fires for a FAILED navigation (documentDidFail); `release` checks
        // the relay's own `loaded` for that case.
        guard landed, gen == generation else {
            coordinator.drop(webView)
            return
        }
        coordinator.release(webView, prerendered: prerendered)
    }

    /// A continuation may be resumed exactly once, and two things race to.
    private final class Once {
        private var done = false
        func run(_ body: () -> Void) {
            guard !done else { return }
            done = true
            body()
        }
    }
}
