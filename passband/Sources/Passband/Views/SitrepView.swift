// SITREP VIEW — the abstracted dashboard and default surface on launch: ranked
// standing items, the reading zone, a status strip and the records rail. Mail still
// landing is a spinner in the masthead (IngestIndicator), not a board zone.
// Owns the "sitrep" KeyContext. No persistent selection — the focus fill renders
// only while the keyboard drives, and hover must NOT drag it.

import SwiftUI

/// How many ranked standing items show before the quiet "{n} more" expander.
private let eyesVisible = BoardFold.forYourEyes

/// THE FOR-YOUR-EYES WALK: the queue the reader is handed when an email is
/// opened from this band, so `E` (done + next) can step through it without
/// coming back here.
///
/// The FULL ranked list, not the visible slice — the cursor stops at the
/// collapse cutoff because arrowing past it is how you ASK for the rest, but a
/// walk that quietly ended at row ten would strand exactly the mail the band is
/// hiding. And the same ranking `body` renders, so the order you see is the
/// order you get.
///
/// Computed at KEY-PRESS AND CLICK TIME ONLY. It re-ranks the standing list,
/// which is the cost `SitrepCursor` exists to keep out of the render path.
///
/// AUTH MAIL IS NOT ON THE WALK. It sits in this band when it needs a ruling
/// (critical, or "Not me"), and from here it opens in the audited one-time
/// reveal, not the reader, so a walk that stepped onto it would leave the walk.
/// Detected by the row's own decision first (`AppStore.authMeta(for:)`).
@MainActor
private func eyesWalk(_ store: AppStore) -> [AttentionUpdate] {
    store.sitrep.standing.filter { store.authMeta(for: $0) == nil }
}

/// The sitrep's hover + keyboard cursor, in an `@Observable` box rather than
/// `SitrepView`'s `@State`.
///
/// WHY IT IS NOT `@State`: a pointer resting anywhere over the page fires hover
/// enter/exit CONTINUOUSLY while a scroll drags rows under it, and a `@State`
/// write at the top of the tree re-renders the whole dashboard for every one of
/// them — re-ranking the standing list, rebuilding every row, re-measuring every
/// reading card. Held here, a write only invalidates views that actually READ
/// that field, and the keymap reads all of it at KEY-PRESS time rather than at
/// render time, so most of these writes reach no observer at all.
@MainActor
@Observable
final class SitrepCursor {
    /// Index into the VISIBLE rows only — collapsed-away rows are unreachable.
    var index = 0
    /// True only while the keyboard is driving; the focus fill renders on this.
    var kbActive = false
    /// True only while the pointer is over a row.
    var hovering = false
    /// Whether "for your eyes" is showing past the first `eyesVisible`.
    var expanded = false
    /// Hovered reading-card address — `e` marks that sender's whole window done,
    /// deferring to the For-your-eyes handler when nothing hovers.
    var reading: String?
    /// Hovered shipment card id — `e` clears that package off the rail until
    /// the carrier or a new email reports something new. Checked before
    /// `reading`; the two can't both be under one pointer.
    var shipment: Int?

    /// Point the cursor at a row the POINTER is over. EVERY WRITE IS GUARDED:
    /// `@Observable` notifies on assignment, not on change, so an unguarded
    /// `kbActive = false` here would invalidate every row on every mouse-move —
    /// which is exactly the cost this whole type exists to avoid.
    func hover(_ index: Int) {
        if !hovering { hovering = true }
        if kbActive { kbActive = false }
        if self.index != index { self.index = index }
    }
}

struct SitrepView: View {
    @Environment(AppStore.self) private var store
    @Environment(Prefs.self) private var prefs
    /// Whether the board is the page on screen. The shell keeps the sitrep
    /// mounted behind other pages (PagePresence), so a visit is this turning
    /// true, not a mount; what a visit does follows it.
    @Environment(\.pageIsActive) private var pageActive

    /// How many of the top rows are worth a pre-rendered frame. Bounded by
    /// WebFramePool's prerendered tier; each is a live content process.
    private static let prerenderRows = 4

    /// The whole warm for a list of board rows: threads first, then frames
    /// for the top few.
    fileprivate static func warmBoard(_ ids: [String]) {
        ThreadPrefetch.shared.warm(ids, immediate: eyesVisible, spacing: .milliseconds(150))
        FramePrewarmer.shared.want(Array(ids.prefix(prerenderRows)))
    }

    /// Hover + keyboard cursor. A reference, deliberately — see `SitrepCursor`.
    @State private var cursor = SitrepCursor()
    /// THE BOARD'S CLOCK, on the half minute. What its own body works out from
    /// the time alone (the records rail emptying as calendar rows end and
    /// today's receipts and deliveries age out) used to ride on the redraw
    /// every poll brought, because the board read the stats and they change
    /// every poll; it no longer does, so the beat is kept here, at the
    /// calendar card's own thirty seconds.
    @State private var clock = Date()

    /// Below this page width the records rail stops being a pinned column and
    /// stacks under the left content, the whole page scrolling as one — a
    /// squeezed side-by-side starves the left column first, and that column is
    /// the work surface. At the window's minimum the page measures ~920, so
    /// the smallest windows always stack.
    ///
    /// Stated as the narrowest work column worth pinning a rail beside (654,
    /// a measure, since it is one-liners that have to fit) plus the rail, so
    /// the comfortable rail's extra width moves the breakpoint with it.
    private static var railBreakpoint: CGFloat { Typo.measure(654) + railWidth }

    /// The records rail. On the spacing curve rather than the type's: a rail
    /// row is a name beside two chips and an amount, and it is the chips'
    /// padding that grows. Held at 306, comfortable left the name no room at
    /// all ("Hertz P…", then a bare "…").
    private static var railWidth: CGFloat { Space.scaled(306) }

    /// Measured page width the breakpoint is judged against. Starts infinite
    /// so the first pass lays out side-by-side rather than flashing stacked.
    @State private var pageWidth: CGFloat = .infinity

    /// The rows the cursor can reach, recomputed at KEY-PRESS time. The render
    /// path does NOT come through here — `body` ranks once into a `let` and
    /// passes the result down.
    private var reachable: [AttentionUpdate] {
        let ranked = store.sitrep.standing
        return cursor.expanded ? ranked : Array(ranked.prefix(eyesVisible))
    }

    var body: some View {
        // ONE rank per render. This was a computed property read FOUR times a
        // pass — twice through `visibleEyes`, and once more by each `onChange`,
        // whose value expression is re-evaluated on every body evaluation — and
        // every read re-sorted the entire standing list.
        let ranked = store.sitrep.standing
        let visible = cursor.expanded ? ranked : Array(ranked.prefix(eyesVisible))
        let overflow = ranked.count - eyesVisible
        let threadIds = ranked.map(\.thread_id)

        return VStack(spacing: 0) {
            masthead
                // The shortcut hints hang below the bar, over the hero.
                .zIndex(1)
            // ABOVE THE HERO, because it outranks it. The hero's question is
            // "what needs you today", and its answer is worthless while the
            // mailbox behind it has been frozen since Tuesday. This is the
            // landing page, so it is where somebody wondering why nothing has
            // arrived actually looks.
            if store.gmailDisconnected {
                GmailDisconnectedBanner()
                    .padding(.horizontal, Space.page)
                    .padding(.top, Space.s14)
            } else if case let status = store.syncStatus, status.showsBanner {
                // Throttled, erroring, catching up, or triage held: the same
                // slot, because it answers the same question.
                SyncStatusBanner(status: status)
                    .padding(.horizontal, Space.page)
                    .padding(.top, Space.s14)
            }
            // THE HERO STAYS PUT. It sits above BOTH columns, so it cannot scroll
            // with one of them and hold still for the other; and "one item needs
            // you today" is the page's standing answer, which is worth keeping on
            // screen rather than scrolling away first.
            // The 18px here is a hard standoff: scrollShadowRoom's top fade
            // reaches zero at the viewport top, so this padding is the band
            // where scrolling content NEVER renders — without it the fade tail
            // touches the headline. The fade zone itself is the matching top
            // padding inside each scroll view's content.
            DashHero(
                standing: store.sitrep.standing,
                headline: store.fyeHeadline,
                regenerating: store.fyeHeadlineRegenerating,
                regenerate: { Task { await store.regenerateFyeHeadline() } },
                // Asked only over an empty board, the one place the hero says
                // it: the line is read off the stats (the sync clock moves
                // every poll), and asking for it always made the whole board
                // a reader of them.
                empty: store.sitrep.standing.isEmpty ? emptyHero : nil)
                .task(id: HeadlineAsk(key: store.headlineKey, onScreen: pageActive)) {
                    guard pageActive else { return }
                    await store.refreshFyeHeadline()
                }
                .padding(.horizontal, Space.page)
                .padding(.bottom, Space.scaled(18))

            if pageWidth < Self.railBreakpoint {
                // NARROW: one scroll, records under the work surface. The rail
                // zones are reference material, so they yield the fold to the
                // items that need acting on — but the status strip stays the
                // page's last line either way.
                ScrollView(.vertical) {
                    VStack(spacing: Space.s16) {
                        leftZones(visible: visible, overflow: overflow)
                        // Same key as the pinned rail below: whichever layout is
                        // on screen is the one the tour's ring finds.
                        if !recordsEmpty {
                            railCards.tourTarget(.records)
                        }
                    }
                    .padding(.top, Space.s14)
                    .padding(.bottom, Space.scaled(28))
                }
                .scrollIndicators(.hidden)
                .scrollShadowRoom(topFade: Space.s14)
                .padding(.horizontal, Space.page)
            } else {
                HStack(alignment: .top, spacing: Space.s16) {
                    // THE ONLY THING THAT SCROLLS WITH THE PAGE.
                    ScrollView(.vertical) {
                        VStack(spacing: Space.s16) {
                            leftZones(visible: visible, overflow: overflow)
                        }
                        .padding(.top, Space.s14)
                        .padding(.bottom, Space.scaled(28))
                    }
                    // No bar on either column. Two of them side by side read as a
                    // split pane rather than one page, and the left one rides the
                    // column boundary instead of the window edge.
                    .scrollIndicators(.hidden)
                    .scrollShadowRoom(topFade: Space.s14)
                    .frame(maxWidth: .infinity, alignment: .top)

                    // THE RECORDS RAIL IS PINNED. These are reference columns you
                    // read WHILE working the left side, so they must not leave with
                    // it. Its own scroll view rather than a plain stack so a rail
                    // taller than the window is still reachable —
                    // `.basedOnSize` is what keeps it completely inert until then,
                    // instead of rubber-banding against nothing.
                    if !recordsEmpty {
                    ScrollView(.vertical) {
                        VStack(spacing: Space.s16) {
                            railZones
                        }
                        .padding(.top, Space.s14)
                        .padding(.bottom, Space.scaled(28))
                    }
                    .scrollBounceBehavior(.basedOnSize)
                    .scrollIndicators(.hidden)
                    .scrollShadowRoom(topFade: Space.s14)
                    .frame(width: Self.railWidth)
                    // The COLUMN, not its content: a rail taller than the window
                    // would put the ring's bottom edge off screen.
                    .tourTarget(.records)
                    }
                }
                .padding(.horizontal, Space.page)
            }
        }
        // The window shell draws the scene (it runs under the rail and the
        // traffic lights), so the sitrep only says whether it should.
        .onChange(of: quiet, initial: true) { _, q in
            if pageActive { QuietBoard.shared.isQuiet = q }
        }
        .onPageShown { QuietBoard.shared.isQuiet = quiet }
        .onPageHidden { QuietBoard.shared.isQuiet = false }
        #if os(macOS)
            // The end of a Space walk: the bottom of the list, celebrated.
            .overlay {
                if let summary = store.walkFinished {
                    // Clears only ITS OWN summary: a celebration replaced by a
                    // newer one has its task cancelled, and its cleanup must
                    // not wipe the one that took its place.
                    WalkComplete(summary: summary, boardCleared: store.sitrep.standing.isEmpty) {
                        if store.walkFinished?.id == summary.id { store.walkFinished = nil }
                    }
                    .id(summary.id)
                }
            }
        #endif
        .animation(.easeInOut(duration: 0.3), value: recordsEmpty)
        .task {
            await clockTicks(every: 30, while: { Platform.isAppOnScreen }) {
                // Not on a fresh mount, which has just drawn with now.
                let now = Date()
                if abs(now.timeIntervalSince(clock)) >= 1 { clock = now }
            }
        }
        .onGeometryChange(for: CGFloat.self) { $0.size.width } action: { pageWidth = $0 }
        .keyContext(.sitrep)
        .keyBindings(.sitrep, bindings)
        // Refreshes underneath what is already on screen — rows are never
        // cleared first, so a revisit paints instantly and updates in place.
        .task(id: pageActive) {
            guard pageActive else { return }
            await store.refreshZones()
        }
        // Warm the visible top-10 immediately and trickle the collapsed
        // remainder, so a long list never stampedes the daemon. Prefetch dedupes
        // in-flight and fresh entries, so re-runs are near-free. The top rows
        // go one step further and get their newest message RENDERED into a
        // parked frame (FramePrewarmer), so the click is a pool hit.
        .onChange(of: threadIds) { _, ids in if pageActive { Self.warmBoard(ids) } }
        .onPageShown { Self.warmBoard(threadIds) }
        // AND KEPT WARM. The warm above ran once per list change, and a
        // cached thread goes stale after its TTL — so a board left open for a
        // few minutes and then clicked paid the round trip the prefetch
        // exists to remove. A zero-size leaf watches the poll heartbeat so
        // this body does not have to.
        .background { if pageActive { BoardWarmKeeper(threadIds: threadIds) } }
        .onChange(of: visible.count) { _, count in
            cursor.index = max(0, min(cursor.index, max(0, count - 1)))
        }
        // Arms the clear moment while there is something on the board, and
        // spends it the first time the board is seen clear afterwards. With
        // `initial`, coming back from the reader after finishing the last
        // email counts: that is where most boards actually get cleared.
        // Keyed on BOTH facts: a board that loads straight into items never
        // changes `isClear` (false before the load, false after), so watching
        // that alone would never arm.
        //
        // NOT WHILE THE BOARD IS A SWITCH'S SNAPSHOT: its rows are what the
        // mailbox held last time, and mail handled somewhere else since would
        // arm on the repaint and chime when the daemon answered. The flag is
        // a key so a snapshot that turns out to be current still arms, on the
        // answer that confirms it.
        //
        // AND SPENT ONLY ON SCREEN. The board is kept mounted behind Emails,
        // so the last email cleared there empties it out of sight; the chime
        // waits for the return, as it did when the return was a remount.
        .onChange(
            of: [
                ranked.isEmpty && readingSenders.isEmpty, quiet, store.boardIsSnapshot, pageActive,
            ], initial: true
        ) { _, now in
            if now[2] { return }
            if !now[0] {
                QuietBoard.shared.armed = true
            } else if now[1], now[3], QuietBoard.shared.armed {
                QuietBoard.shared.armed = false
                ClearChime.play()
            }
        }
    }

    /// What the hero says over an empty board that is empty for a reason other
    /// than the reader having handled everything. Nil when the board is clear.
    private var emptyHero: (title: String, note: String)? {
        store.syncStatus.emptyHero(queued: store.triageWaiting, lastSync: store.daemonLastSync)
    }

    /// Nothing for your eyes, and truly so: the board has loaded, and it is not
    /// empty only because sync or triage is stuck.
    private var isClear: Bool {
        store.hasStats && store.sitrep.standing.isEmpty && emptyHero == nil
    }

    /// The reading cards as the zone draws them.
    private var readingSenders: [ReadingSender] {
        store.readingOnBoard
    }

    /// Clear AND nothing to read: the board with nothing on it at all, which
    /// is what the quiet board's scene plays behind. Mail in either zone
    /// fades it away. "Nothing to read" has to be an ANSWER: the stats land
    /// a round trip before the reading feed does, and in that gap a mailbox
    /// with only reading mail looked quiet, scene, chime and all.
    private var quiet: Bool {
        isClear && store.zones.readingLoaded && readingSenders.isEmpty
    }

    /// Every records card would only say it has nothing. Four cards of "No
    /// receipts today." is a dashboard of absences, so the rail steps aside.
    private var recordsEmpty: Bool {
        _ = clock
        return CalendarZone.rows(store, now: Date()).isEmpty
            && ShipmentsZone.rows(store).isEmpty
            && BankingZone.rows(store).isEmpty
            && ReceiptsZone.rows(store).isEmpty
    }

    // MARK: - columns

    /// The work-surface zones, shared by both layouts. The status strip is NOT
    /// here — each layout places it last on the page itself.
    @ViewBuilder
    private func leftZones(visible: [AttentionUpdate], overflow: Int) -> some View {
        if !store.sitrep.standing.isEmpty {
            forYourEyes(visible: visible, overflow: overflow)
                .tourTarget(.eyes)
        } else if isClear {
            ClearNote()
                .transition(.opacity)
        }
        // Empty reading still stands beside a live For-your-eyes (a zone that
        // vanishes mid-work reads as a missing feature), but on a board with
        // nothing else it is one more absence to look at.
        let senders = readingSenders
        if !(senders.isEmpty && store.sitrep.standing.isEmpty) {
            ReadingZone(senders: senders, cursor: cursor)
                .tourTarget(.reading)
        }
    }

    /// The records zones as the pinned rail shows them: full-width rows.
    @ViewBuilder
    private var railZones: some View {
        CalendarZone().tourTarget(.calendar)
        ShipmentsZone(cursor: cursor).tourTarget(.shipments)
        BankingZone().tourTarget(.banking)
        ReceiptsZone().tourTarget(.receipts)
    }

    /// The records as HALF-WIDTH cards for the stacked layout. Two top-aligned
    /// columns rather than a LazyVGrid: a grid row takes its tallest cell's
    /// height, and these cards are never the same height, so a grid opens
    /// gaps that independent columns simply pack.
    private var railCards: some View {
        HStack(alignment: .top, spacing: Space.s16) {
            VStack(spacing: Space.s16) {
                CalendarZone().tourTarget(.calendar)
                BankingZone().tourTarget(.banking)
            }
            .frame(maxWidth: .infinity)
            VStack(spacing: Space.s16) {
                ShipmentsZone(cursor: cursor).tourTarget(.shipments)
                ReceiptsZone().tourTarget(.receipts)
            }
            .frame(maxWidth: .infinity)
        }
    }

    // MARK: - masthead

    /// The branch a dev build was cut from, or nil for release builds, builds
    /// from main, and anything that never went through build.sh.
    private static let buildBranch: String? = {
        let raw = Bundle.main.infoDictionary?["PassbandBuildBranch"] as? String ?? ""
        let branch = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        return branch.isEmpty ? nil : branch
    }()

    private var masthead: some View {
        HStack(alignment: .firstTextBaseline, spacing: Space.s10) {
            HStack(alignment: .firstTextBaseline, spacing: Space.scaled(7)) {
                Text("passband")
                    .font(Typo.serif(19, weight: .medium))
                    .foregroundStyle(Palette.ink)
                Text("sitrep")
                    .font(Typo.micro)
                    .foregroundStyle(Palette.inkFaintest)
                    .textCase(.uppercase)
                if let branch = Self.buildBranch {
                    // Dev builds only: which branch this binary came from, so
                    // a desk with several test builds open can tell them
                    // apart. Mono and tinted so it reads as a build stamp,
                    // not product copy. Release builds and main leave the
                    // plist key empty (see build.sh) and get nothing.
                    Text(branch)
                        .font(Typo.mono(10, weight: .medium))
                        .foregroundStyle(Palette.warn)
                        .padding(.horizontal, Space.s6)
                        .padding(.vertical, Space.s2)
                        .background(Palette.warnSoft, in: Capsule())
                        .help("Built from branch \(branch)")
                        .accessibilityLabel("Development build from branch \(branch)")
                }
            }
            Spacer(minLength: Space.s12)
            IngestIndicator()
            // Only the practice board gets the rehearsal appearance control.
            // The launch flag stays true after returning to the real inbox.
            if RehearsalMode.launchedStandalone && RehearsalMode.isEnabled {
                DemoAppearancePicker()
            }
            RetriageButton()
            #if os(macOS)
                // The practice board has no `/` (the global set is cut down to
                // undo + theme there), so an icon for it would be a button
                // that does nothing. The phone has its own sitrep.
                if !RehearsalMode.isEnabled {
                    MastheadShortcuts()
                }
            #endif
        }
        .padding(.horizontal, Space.page)
        // THE TOP BAR. The wordmark sits on the traffic lights' line rather than
        // under it, which is why this is a fixed height and not padding: the
        // rail beside it starts at the same line, and the two only read as one
        // bar if neither can drift.
        .frame(height: TopBar.height)
    }

    // MARK: - (a) for your eyes

    /// The standing band, ranked: dated obligations AND live correspondence
    /// (threads the reader has written in, senders they have written to). A row
    /// here is therefore no promise of a deadline — `Ranking` scores a dateless
    /// row at urgency 0, which is what keeps the real dates at the top.
    private func forYourEyes(visible: [AttentionUpdate], overflow: Int) -> some View {
        ZoneCard(
            symbol: "eye", title: "For your eyes", count: store.sitrep.standing.count,
            trailing: store.sitrep.standing.isEmpty
                ? nil
                : AnyView(
                    FlowModeButton {
                        // Exactly what Space does from a board with no row
                        // highlighted: the first row, the whole band as the walk.
                        let walk = eyesWalk(store)
                        guard let first = walk.first else { return }
                        store.startWalk(first.thread_id, queue: walk)
                    })
        ) {
            VStack(spacing: 1) {
                ForEach(Array(visible.enumerated()), id: \.element.id) { i, u in
                    // No closures passed down: a stored closure is never equal to
                    // last render's, so handing rows their actions that way meant
                    // SwiftUI could not skip a single one when the parent redrew.
                    if let meta = store.authMeta(for: u) {
                        AuthEyesRow(update: u, meta: meta, index: i, cursor: cursor)
                    } else if RehearsalMode.isEnabled && u.id == 1 {
                        ObligationRow(update: u, index: i, cursor: cursor)
                            .tourTarget(.maya)
                    } else if RehearsalMode.isEnabled && u.id == 11 {
                        ObligationRow(update: u, index: i, cursor: cursor)
                            .tourTarget(.brightly)
                    } else {
                        ObligationRow(update: u, index: i, cursor: cursor)
                    }
                }
                if overflow > 0 { expander(overflow) }
            }
            // Only LEAVING the zone ends the hover — crossing between rows never
            // fires a parent's hover, so a sweep still can't flicker actionable
            // off and on. This was `.onContinuousHover`, which delivers every
            // mouse-moved event over the whole zone; it only ever needed .ended,
            // and the rest was a stream of hit-tests during the exact scroll
            // this zone is supposed to stay out of the way of.
            .onHover { over in
                if !over, cursor.hovering { cursor.hovering = false }
            }
        }
    }

    /// Shared by the button and by arrowing off the end of the collapsed list,
    /// so both openings look like the same gesture.
    private static var expandAnimation: Animation { ZoneExpander.animation }

    private func expander(_ overflow: Int) -> some View {
        ZoneExpander(overflow: overflow, expanded: cursor.expanded) {
            withAnimation(Self.expandAnimation) { cursor.expanded.toggle() }
        }
    }

    // MARK: - keymap

    /// Action keys are inert unless something is actually highlighted — either
    /// the keyboard cursor or a live hover. Read from HANDLERS only, never from
    /// `body`, which is what keeps these fields free to change on hover.
    private var eyesActionable: Bool { cursor.kbActive || cursor.hovering }

    private var bindings: [KeyBinding] {
        [
            KeyBinding("j", "next item") { moveEyes(+1) },
            KeyBinding("k", "prev item") { moveEyes(-1) },
            KeyBinding("ArrowDown", "next item") { moveEyes(+1) },
            KeyBinding("ArrowUp", "prev item") { moveEyes(-1) },
            KeyBinding("d", "mark done") {
                guard eyesActionable, let u = reachable[safe: cursor.index] else { return }
                Task { await Actions.done(u, via: .key, on: .sitrep) }
            },
            // `e` first tries the hovered shipment, then the hovered reading
            // card; with nothing hovered it falls to the for-your-eyes row.
            KeyBinding(declining: "e", "mark done") {
                // Checked against the rail as DRAWN: a card that left it (its
                // own clear, or a refresh) never sends onHover(false), so a
                // stale id must fall through rather than clear something unseen.
                if let id = cursor.shipment,
                    ShipmentsZone.rows(store).contains(where: { $0.id == id })
                {
                    cursor.shipment = nil
                    Task { await store.clearShipment(id) }
                    return true
                }
                if let addr = cursor.reading,
                    let rs = store.zones.reading.first(where: { $0.address == addr })
                {
                    Task { await markReadingDone(rs) }
                    return true
                }
                guard eyesActionable, let u = reachable[safe: cursor.index] else { return false }
                Task { await Actions.done(u, via: .key, on: .sitrep) }
                return true
            },
            KeyBinding("Enter", "open email") {
                guard eyesActionable, let u = reachable[safe: cursor.index] else { return }
                // Auth mail opens in the audited one-time reveal.
                if let meta = store.authMeta(for: u) {
                    store.requestReveal(meta)
                    return
                }
                store.openThread(u.thread_id, queue: eyesWalk(store), from: .sitrep)
            },
            // Space STARTS the walk: no highlight needed, so it is the one key
            // that works the moment the page opens. A row the cursor is on still
            // wins, so it never jumps back past where you arrowed to. Declines on
            // an empty board so Space keeps scrolling the page.
            KeyBinding(declining: "Space", "start with the first item") {
                let walk = eyesWalk(store)
                let u = eyesActionable ? reachable[safe: cursor.index] : walk.first
                guard let u else { return false }
                // A highlighted auth row opens the reveal, not a walk.
                if let meta = store.authMeta(for: u) {
                    store.requestReveal(meta)
                    return true
                }
                store.startWalk(u.thread_id, queue: walk)
                return true
            },
            // Same guard as every other verb here — inert unless a row is
            // actually highlighted. Reply opens the email and composes inside it,
            // and hands over the same walk Enter does, so `E` keeps working from
            // inside the reader once the reply is away.
            KeyBinding("r", "reply") {
                guard eyesActionable, let u = reachable[safe: cursor.index] else { return }
                // Replying opens the reader on the sealed body; from here, an
                // auth row only ever opens the reveal.
                guard store.authMeta(for: u) == nil else { return }
                Actions.reply(u, queue: eyesWalk(store))
            },
            KeyBinding("v", "fix triage") {
                guard eyesActionable, let u = reachable[safe: cursor.index] else { return }
                // Inert on an auth row: it opens only in the reveal and leaves
                // only by an answer or Done (remind would park it).
                guard store.authMeta(for: u) == nil else { return }
                store.openTriageFix(
                    TriageFixTarget(
                        messageId: u.id, sender: u.sender, subject: u.one_line))
            },
            // `t` = tune this sender's rule, the verb the reader has on the
            // same key. Bound HERE because the sitrep is its own key context:
            // the list's `t` is never consulted from this page.
            KeyBinding("t", "tune sender") {
                guard eyesActionable, let u = reachable[safe: cursor.index] else { return }
                // Inert on an auth row: it opens only in the reveal and leaves
                // only by an answer or Done (remind would park it).
                guard store.authMeta(for: u) == nil else { return }
                Actions.tune(sender: u.sender)
            },
            KeyBinding("h", "remind me later") {
                guard eyesActionable, let u = reachable[safe: cursor.index] else { return }
                // Inert on an auth row: it opens only in the reveal and leaves
                // only by an answer or Done (remind would park it).
                guard store.authMeta(for: u) == nil else { return }
                store.openRemind(
                    RemindTarget(
                        messageId: u.id, sender: u.sender, subject: u.one_line,
                        remindAt: u.remind_at))
            },
            // The one verb here that is NOT about a highlighted row: a new message
            // needs nothing selected, so it skips the `eyesActionable` guard.
            KeyBinding("c", "new message") { store.openComposeNew() },
        ]
    }

    /// j/k hand the cursor back to the keyboard, which is what makes the focus
    /// glass reappear.
    private func moveEyes(_ delta: Int) {
        cursor.kbActive = true
        // Ranked once here rather than through `reachable` twice: the expander
        // check needs the full list and the cursor needs the visible one.
        let ranked = store.sitrep.standing
        let rows = cursor.expanded ? ranked : Array(ranked.prefix(eyesVisible))
        let next = cursor.index + delta

        // DOWN OFF THE END OF A COLLAPSED LIST OPENS IT and keeps going, landing
        // on the first newly-revealed row. Without this the expander is the one
        // control in the zone that only a mouse can reach, and the cursor just
        // stalls on the last row with no indication there is more beneath it.
        if delta > 0, next >= rows.count, !cursor.expanded, ranked.count > rows.count {
            withAnimation(Self.expandAnimation) { cursor.expanded = true }
            cursor.index = rows.count
            return
        }
        guard !rows.isEmpty else { return }
        // Off either end of the list as drawn WRAPS: down from the last row
        // goes back to the top, up from the first goes to the bottom.
        cursor.index = (next % rows.count + rows.count) % rows.count
    }

    private func markReadingDone(_ rs: ReadingSender) async {
        await store.markReadingDone(rs.items, sender: rs.sender)
    }

}

/// MAIL LANDING, as a spinner in the masthead rather than a card on the board.
///
/// The signal is the `new` band — `surfaced_at IS NULL` — and "surfaced" means
/// the client has FETCHED the row: `get_updates` stamps the very page it
/// returns. So a freshly triaged message sits here for about one poll and is
/// then gone, having moved to wherever its tier belongs.
///
/// That one-poll window is exactly why this must not be a zone card. A card
/// announces arriving mail as anonymous sender chips a beat BEFORE the same
/// mail takes its real place in For-your-eyes — the same item, twice, under a
/// name derived from its sending domain. It is a transient STATUS, not a
/// destination, so it reads as one: no count of things to go look at, no click
/// target that vanishes while you reach for it.
private struct IngestIndicator: View {
    @Environment(AppStore.self) private var store

    var body: some View {
        let count = store.sitrep.new.count
        if count > 0 {
            HStack(spacing: Space.scaled(5)) {
                Image(systemName: "arrow.triangle.2.circlepath")
                    .font(Typo.ui(10, weight: .semibold))
                    .symbolEffect(.rotate, isActive: true)
                Text(count == 1 ? "ingesting" : "ingesting \(count)")
            }
            .font(Typo.chip)
            .foregroundStyle(Palette.accent)
            .padding(.horizontal, Space.scaled(9))
            .padding(.vertical, Space.scaled(3))
            .glassCapsule(tint: Palette.accent.opacity(0.18), interactive: false)
            .help(
                count == 1
                    ? "a new email is being triaged; it lands in its band on the next refresh"
                    : "\(count) new emails are being triaged; they land in their bands on the next refresh"
            )
        }
    }
}

// MARK: - GREETING

/// "GOOD MORNING, BRAELYN" — the one line on either client that says the human's
/// own name, and the one place they can set it without going to Settings.
///
/// THE NAME IS A GUESS UNTIL IT ISN'T. A fresh install has no name at all, and
/// nobody opens Settings to introduce themselves, so the greeting used to read
/// as an anonymous "GOOD MORNING" forever. It is now seeded from the mailbox's
/// local part the first time the daemon names the account
/// (`Prefs.seedUserName`), and a seed is a claim about someone that they never
/// made — so it comes with a pencil, and the pencil edits IN PLACE rather than
/// routing to Settings, because the thing being corrected is right there.
///
/// The affordance is deliberately SINGLE-USE: it shows only while
/// `prefs.nameChosen` is false, and the first answer — typed here or into the
/// Settings row — retires it for good. A permanent edit button on a greeting is
/// chrome; this is a question the app owes the human exactly once.
///
/// Shared by both clients (the Mac's `DashHero`, the phone's hero), so the
/// nudge cannot ship on one surface and rot on the other.
struct GreetingLine: View {
    @Environment(Prefs.self) private var prefs

    /// The edit buffer. Never bound straight to the pref: a live binding would
    /// mark the name CHOSEN on the first keystroke, which is precisely the
    /// state this view exists to end deliberately.
    @State private var draft = ""
    @State private var editing = false
    @FocusState private var focused: Bool

    var body: some View {
        HStack(spacing: Space.s6) {
            if editing {
                label(Self.greeting() + ",")
                field
            } else {
                label(
                    Self.greeting() + (prefs.userName.isEmpty ? "" : ", \(prefs.userName)"))
                if !prefs.nameChosen { pencil }
            }
        }
        #if os(macOS)
            // Esc closes this, like everything else. It has to be a BINDING and
            // not `.onExitCommand`: the app's key monitor sees the event first
            // and Escape is the one key it never suppresses for a focused text
            // field, so a surface that wants its own Escape must claim it.
            // Registered in `.sitrep` (which outranks `.global` in dispatch
            // order) and only while the editor is up.
            .keyBindings(
                .sitrep,
                editing ? [KeyBinding("Escape", "cancel", allowInInput: true) { close() }] : []
            )
        #endif
        // The board is kept mounted behind other pages: an editor left open
        // there would keep the keyboard on a field nobody can see.
        .onPageHidden { if editing { close() } }
    }

    private func label(_ text: String) -> some View {
        Text(text)
            .font(Typo.micro)
            .foregroundStyle(Palette.accent)
            .textCase(.uppercase)
            .tracking(0.6)
    }

    private var field: some View {
        TextField("your name", text: $draft)
            .textFieldStyle(.plain)
            .autocorrectionDisabled()
            .focused($focused)
            .font(Typo.micro)
            .foregroundStyle(Palette.accent)
            .tracking(0.6)
            .frame(maxWidth: Typo.measure(160))
            // AN OVERLAY, not a border: it has to say "this is a field" without
            // costing a single point of height, or the serif headline under it
            // would hop down and back every time the editor opens and closes.
            .overlay(alignment: .bottom) {
                Rectangle()
                    .fill(Palette.accent.opacity(0.4))
                    .frame(height: 1)
                    .offset(y: 3)
            }
            .onSubmit(commit)
            // BLUR CANCELS, which is the opposite of the Settings rows' "click
            // away saves" — and for the opposite reason. Saving on blur would
            // commit a half-typed name AND retire the pencil that is the only
            // way to see the mistake, so clicking away costs a retyped word
            // rather than a wrong name you have to hunt through Settings to fix.
            .onChange(of: focused) { _, nowFocused in
                if !nowFocused { editing = false }
            }
            .submitLabel(.done)
            .onAppear { focused = true }
    }

    private var pencil: some View {
        Button {
            draft = prefs.userName
            editing = true
        } label: {
            Image(systemName: "pencil")
                .font(Typo.ui(10, weight: .semibold))
                .foregroundStyle(Palette.accent.opacity(0.8))
                // Clickable past the glyph, but NOT past the line: a target
                // taller than the label it sits beside would push the serif
                // headline down by the height of a pencil nobody can see.
                .padding(Space.scaled(3))
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel("edit your name")
        #if os(macOS)
            // Honest either way. With nothing seeded — a daemon too old to name
            // its mailbox — there is no guess to correct, only something to say.
            .help(
                prefs.userName.isEmpty
                    ? "say what this greeting should call you"
                    : "this is a guess from your email address; click to say what to call you"
            )
        #endif
    }

    /// Enter saves. An EMPTY box is a cancel, not an erasure: a name cleared
    /// from here would take the pencil with it and leave Settings as the only
    /// way back, which is a lot to charge for a stray Enter.
    private func commit() {
        let typed = draft.trimmed
        close()
        guard !typed.isEmpty else { return }
        // The setter is what records the name as CHOSEN — see Prefs.userName.
        prefs.userName = typed
    }

    /// Both exits go through here. Dropping `focused` EXPLICITLY rather than
    /// leaning on SwiftUI to reset it when the field unmounts: cancel is the
    /// one exit that leaves the pencil on screen, so it is the one exit with a
    /// second open behind it, and a `focused` still stuck true would make that
    /// open's `.onAppear` a no-op — a field you click into and cannot type in.
    private func close() {
        focused = false
        editing = false
    }

    static func greeting(now: Date = Date()) -> String {
        let h = Calendar.current.component(.hour, from: now)
        if h < 12 { return "Good morning" }
        if h < 18 { return "Good afternoon" }
        return "Good evening"
    }
}

// MARK: - DASH HERO

/// A greeting label plus one serif headline saying what the reader's
/// For-your-eyes band is about (see `HeroLine`).
/// The ONE place the display serif appears — held to a single line per screen so
/// it reads as voice rather than decoration.
private struct DashHero: View {
    let standing: [AttentionUpdate]
    /// The daemon's sentence about the band, when it has written one.
    var headline: String? = nil
    var regenerating = false
    /// Clicking the line asks for another one.
    var regenerate: () -> Void = {}
    /// What to say over an empty board when it is empty because triage has
    /// not run (see `SyncStatus.emptyHero`). Nil lets "You're all clear." stand.
    var empty: (title: String, note: String)? = nil

    private var title: String {
        HeroLine.title(headline: headline, count: standing.count, empty: empty?.title)
    }

    /// The reason under an empty-board headline, only when there is one.
    private var note: String? { standing.isEmpty ? empty?.note : nil }

    var body: some View {
        VStack(alignment: .leading, spacing: Space.s2) {
            GreetingLine()
            Text(title)
                .font(Typo.hero(38))
                .foregroundStyle(Palette.ink)
                .lineSpacing(-2)
                .fixedSize(horizontal: false, vertical: true)
                // The model's sentence lands a beat after the board does; let
                // it fade in over the fallback instead of snapping.
                .contentTransition(.opacity)
                .animation(.easeInOut(duration: 0.35), value: title)
                // Click for another line. Only over a board with mail on it:
                // the empty-board lines are not the model's to rewrite.
                .opacity(regenerating ? 0.45 : 1)
                .animation(.easeInOut(duration: 0.2), value: regenerating)
                .contentShape(Rectangle())
                .onTapGesture { if !standing.isEmpty { regenerate() } }
                .help(standing.isEmpty ? "" : "Click for another")
                #if os(macOS)
                    .pointerStyle(standing.isEmpty ? nil : .link)
                #endif
            if let note {
                Text(note)
                    .font(Typo.rowSub)
                    .foregroundStyle(Palette.inkDim)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .padding(.top, Space.s4)
        .padding(.bottom, Space.s2)
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

#if os(macOS)
    // MARK: - masthead shortcuts

    /// Compose and search, at the top bar's right end.
    /// Both are one keystroke away already; these are for the reader who has
    /// not learned the keys yet, and hovering is how they learn them.
    private struct MastheadShortcuts: View {
        @Environment(AppStore.self) private var store

        var body: some View {
            HStack(spacing: Space.s2) {
                MastheadShortcut(symbol: "square.and.pencil", label: "new message", key: "c") {
                    store.openComposeNew()
                }
                MastheadShortcut(symbol: "magnifyingglass", label: "search", key: "/") {
                    store.openSearch()
                }
            }
            // The bar and the records rail are both inset 24, so pull the
            // cluster out until the last GLYPH (not its 30pt hit tile) lands
            // on the rail's right edge.
            .padding(.trailing, -6)
        }
    }

    private struct MastheadShortcut: View {
        let symbol: String
        let label: String
        let key: String
        let action: () -> Void

        @State private var hovering = false

        var body: some View {
            Button(action: action) {
                Image(systemName: symbol)
                    .font(Typo.ui(17, weight: .regular))
                    .foregroundStyle(hovering ? Palette.ink : Palette.inkFaint)
                    .frame(width: 30, height: 30)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .background {
                RoundedRectangle(cornerRadius: Radius.chip, style: .continuous)
                    .fill(Palette.hairline.opacity(hovering ? 0.7 : 0))
            }
            // Our own hint rather than `.help`: the system tooltip waits most
            // of a second and cannot draw a key cap, and the key is the point.
            // Trailing-anchored: these sit on the window's right edge, and a
            // centered hint under the last icon would run off it.
            .overlay(alignment: .topTrailing) {
                HStack(spacing: Space.scaled(5)) {
                    Text(label)
                        .font(Typo.micro)
                        .foregroundStyle(Palette.inkDim)
                    Kbd(key)
                }
                .padding(.horizontal, Space.scaled(7))
                .padding(.vertical, Space.s4)
                .background(
                    RoundedRectangle(cornerRadius: Radius.chip, style: .continuous)
                        .fill(Palette.canvas)
                )
                .overlay(
                    RoundedRectangle(cornerRadius: Radius.chip, style: .continuous)
                        .strokeBorder(Palette.hairline, lineWidth: 0.5)
                )
                .fixedSize()
                .offset(y: 34)
                .opacity(hovering ? 1 : 0)
                .allowsHitTesting(false)
            }
            .animation(.easeOut(duration: 0.12), value: hovering)
            .onHover { hovering = $0 }
            .pointingHand()
            .accessibilityLabel(label)
            .accessibilityHint("shortcut \(key)")
        }
    }
#endif

/// Re-warms the board's threads on every poll heartbeat. A LEAF of its own
/// so that reading `lastRefresh` — which changes on every poll — costs a
/// zero-size view's body and not the board's.
private struct BoardWarmKeeper: View {
    @Environment(AppStore.self) private var store
    let threadIds: [String]

    var body: some View {
        Color.clear
            .frame(width: 0, height: 0)
            // THE VISIBLE ROWS ONLY. The whole band is warmed when the list
            // changes; re-warming all two hundred every tick would start a
            // thirty-second trickle every ten seconds and, when the TTL
            // lapsed, refetch every collapsed row nobody can click.
            .onChange(of: store.lastRefresh) { _, _ in
                SitrepView.warmBoard(Array(threadIds.prefix(eyesVisible)))
            }
    }
}

// MARK: - auth row

/// Auth mail in For your eyes: a critical account change nobody has ruled on,
/// or one the owner said was not them. Its body is sealed, so this row shows
/// the model's one-line summary and a lock, never prefetches the thread
/// (`ThreadPrefetch.refuses`), and opens the audited one-time reveal. The
/// human thread view would serve it; this row simply never asks.
private struct AuthEyesRow: View {
    @Environment(AppStore.self) private var store
    let update: AttentionUpdate
    let meta: SealedMeta
    let index: Int
    let cursor: SitrepCursor

    @State private var hover = RowHover()

    private var tone: Color { meta.isCritical ? Palette.danger : Palette.lock }

    var body: some View {
        Button {
            cursor.index = index
            store.requestReveal(meta)
        } label: {
            HStack(spacing: Space.scaled(9)) {
                Avatar(sender: update.senderString, size: 22)
                Text(SenderCache.resolved(update.senderString).displayName)
                    .font(Typo.ui(12, weight: .medium))
                    .foregroundStyle(Palette.ink)
                    .lineLimit(1)
                    .layoutPriority(2)
                Image(systemName: "lock.fill")
                    .font(Typo.ui(10))
                    .foregroundStyle(tone)
                    .help("Sealed. Opening it reveals it once and writes an audit entry.")
                Text(update.one_line.isEmpty ? AuthCopy.label(meta.kind) : update.one_line)
                    .font(Typo.rowSub)
                    .foregroundStyle(Palette.inkDim)
                    .lineLimit(1)
                    .truncationMode(.tail)
                    .frame(maxWidth: .infinity, alignment: .leading)
                Chip(
                    text: meta.isCritical ? "Critical" : AuthCopy.label(meta.kind),
                    tone: tone,
                    symbol: meta.isCritical ? "exclamationmark.triangle.fill" : AuthCopy.symbol(meta.kind),
                    filled: meta.isCritical
                )
                .layoutPriority(3)
            }
            .padding(.horizontal, EyesRow.hPadding)
            .padding(.vertical, Space.scaled(7))
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .background {
            ObligationWash(index: index, cursor: cursor, hover: hover, tint: tone)
        }
        .padding(.horizontal, -EyesRow.hPadding)
        .onHover { over in
            hover.on = over
            if over { cursor.hover(index) }
        }
        .accessibilityLabel(
            "\(AuthCopy.label(meta.kind)) from \(SenderCache.resolved(update.senderString).displayName), sealed")
    }
}

// MARK: - obligation row

/// A For-your-eyes row's inset. The row is then pulled back out by the same
/// amount (DeskRow's idea), so its avatar sits on the zone title's line and
/// the hover/selection wash spends the card's padding instead of indenting it.
@MainActor
private enum EyesRow {
    static var hPadding: CGFloat { Space.s10 }
}

private struct ObligationRow: View {
    @Environment(AppStore.self) private var store
    /// READ AT CLICK TIME ONLY, never in `body` — an Observable property read
    /// during a render pass would subscribe all 10 rows to the rank slider.
    @Environment(Prefs.self) private var prefs
    let update: AttentionUpdate
    let index: Int
    let cursor: SitrepCursor

    /// Hover, in a REFERENCE this body never reads — see `ObligationWash`. The
    /// row deliberately reads neither hover nor focus, so neither can relayout it.
    @State private var hover = RowHover()

    /// Best-effort money amount from an update's one_line ("$142.00"). Hand-
    /// scanned and memoized rather than regex: this runs for every visible row
    /// on every render, and Swift `Regex` is far too slow for that path.
    private var amount: String? { MoneyScan.amount(in: update.one_line) }

    /// Whether this row's TIER asserts a date. Reading the tier rather than the
    /// band is the point: the band admits dateless mail on its own terms, so
    /// only a tier that promised a date can be missing one.
    private var claimsDate: Bool { update.tier == .pastDue || update.tier == .deadline }

    var body: some View {
        let chip = Fmt.deadlineChip(update.displayDeadline)
        let overdue = chip?.overdue ?? false

        // Click anywhere on the row opens the email; done is keyboard-only (e/d).
        // The walk rides along with the click too — how you opened an email must
        // not decide whether `E` inside it has anywhere to go.
        Button {
            cursor.index = index
            store.openThread(update.thread_id, queue: eyesWalk(store), from: .sitrep)
        } label: {
            HStack(spacing: Space.scaled(9)) {
                Avatar(sender: update.senderString, size: 22)
                Text(SenderCache.resolved(update.senderString).displayName)
                    .font(Typo.ui(12, weight: .medium))
                    .foregroundStyle(Palette.ink)
                    .lineLimit(1)
                    .layoutPriority(2)
                if update.hasAttachments {
                    Image(systemName: "paperclip")
                        .font(Typo.ui(10))
                        .foregroundStyle(Palette.inkFaintest)
                }
                // The abstracted one-liner carries the meaning; it truncates first.
                Text(update.one_line)
                    .font(Typo.rowSub)
                    .foregroundStyle(Palette.inkDim)
                    .lineLimit(1)
                    .truncationMode(.tail)
                    .frame(maxWidth: .infinity, alignment: .leading)
                if let amount {
                    Label(amount, systemImage: "receipt")
                        .font(Typo.num(11, weight: .medium))
                        .foregroundStyle(Palette.accentInk)
                        .labelStyle(.titleAndIcon)
                        .layoutPriority(2)
                }
                if let chip {
                    Chip(
                        text: chip.text, tone: overdue ? Palette.danger : Palette.warn,
                        filled: overdue
                    )
                    .layoutPriority(3)
                } else if claimsDate {
                    // Said ONLY where the tier asserts a date the row does not
                    // carry. Most of this zone is dateless by design — live
                    // correspondence — and stamping every one of those rows
                    // "no due date" annotates an omission they never had.
                    Text("no due date")
                        .font(Typo.micro)
                        .foregroundStyle(Palette.inkFaintest)
                        .help(update.field_reasons?.deadline ?? "")
                        .layoutPriority(3)
                }
            }
            .padding(.horizontal, EyesRow.hPadding)
            .padding(.vertical, Space.scaled(7))
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        // A BACKGROUND, not a modifier on this body: a background is sized by its
        // primary content, so the wash repainting can never resize the row.
        .background {
            ObligationWash(
                index: index, cursor: cursor, hover: hover,
                tint: overdue ? Palette.danger : Palette.accent)
        }
        .overlay(alignment: .leading) {
            if overdue {
                RoundedRectangle(cornerRadius: 1)
                    .fill(Palette.danger)
                    .frame(width: 2)
                    .padding(.vertical, Space.scaled(5))
            }
        }
        .padding(.horizontal, -EyesRow.hPadding)
        .onHover { over in
            hover.on = over
            if over {
                cursor.hover(index)
                // The pointer resting on a row is the best predictor of the
                // next click: make sure its thread is cached (a no-op when it
                // already is) and, a beat later, rendered.
                ThreadPrefetch.shared.prefetch(update.thread_id, fresh: ThreadPrefetch.warmFresh)
                FramePrewarmer.shared.nudge(update.thread_id)
            }
        }
    }
}

/// One row's hover, as a reference so the row's own body never reads it.
@MainActor
@Observable
private final class RowHover {
    var on = false
}

/// The row's selection + hover paint, as a LEAF placed in `.background`.
///
/// WHY IT IS NOT PART OF THE ROW'S BODY. A `sample` taken during a stuttering
/// scroll put the main thread overwhelmingly in stack SIZING —
/// `LayoutEngineBox.explicitAlignment`, `StackLayout.prioritize`,
/// `ViewDimensions.subscript` — and almost nowhere in view updates. So the cost
/// of a hover was never the re-render; it was the RELAYOUT the re-render
/// triggers. An obligation row is a stack of stacks carrying three
/// layout-priority bands and a `Label`, and re-running its body re-measures all
/// of that, then propagates up through the zone card and the column. A
/// reading card, which has neither priorities nor a Label, went smooth as
/// soon as its parent stopped redrawing; these rows did not, and this is why.
///
/// Down here nothing above can be resized: a background is sized by its primary
/// content, so repainting this is paint and nothing else.
private struct ObligationWash: View {
    let index: Int
    let cursor: SitrepCursor
    let hover: RowHover
    let tint: Color

    private static let radius: CGFloat = Radius.row

    /// Short-circuits exactly as it did on the row: with the keyboard idle
    /// `cursor.index` is never read, so the index write every hover performs
    /// still reaches no observer.
    private var focused: Bool { cursor.kbActive && cursor.index == index }

    var body: some View {
        RoundedRectangle(cornerRadius: Self.radius, style: .continuous)
            .fill(
                focused
                    ? tint.opacity(SelectionTone.selected)
                    : (hover.on ? Palette.hairline.opacity(SelectionTone.hover) : .clear)
            )
            .overlay(
                RoundedRectangle(cornerRadius: Self.radius, style: .continuous)
                    .strokeBorder(
                        focused ? tint.opacity(SelectionTone.border) : .clear, lineWidth: 1)
            )
    }
}

// MARK: - dev re-triage button

/// DEV-MODE re-triage: renders nothing unless the developerMode pref is on.
/// ASKS FIRST (`RetriageConfirm`), then fires POST /client/retriage for the
/// trailing 7 days and hands the window to `RetriageModal`, which blocks the app
/// until the queues drain — the run rewrites every tier on the board, so there is
/// nothing here worth reading while it happens. The confirm exists because of
/// where this chip sits: a text control in a chrome bar, next to the sync stamp
/// you actually click, in front of a wait you cannot cancel (#210).
///
/// `busy` is the STORE's run, not a local flag: the modal outlives this button
/// (a re-triage kicked from the sitrep survives navigating away), so the only
/// honest source for "already going" is the run itself.
struct RetriageButton: View {
    @Environment(AppStore.self) private var store
    @Environment(Prefs.self) private var prefs

    private static let days = 7

    private var busy: Bool { store.retriage != nil }

    var body: some View {
        if prefs.developerMode {
            Button {
                guard !busy else { return }
                store.askRetriage(days: Self.days)
            } label: {
                Label("re-triage 7d", systemImage: "arrow.trianglehead.2.clockwise")
                    .font(Typo.micro)
                    .symbolEffect(.rotate, isActive: busy)
                    .padding(.horizontal, Space.s8)
                    .padding(.vertical, Space.scaled(3))
            }
            // Text-only: an outlined pill would put the loudest shape on the
            // page around a dev control most readers never use.
            .buttonStyle(.textAction)
            .disabled(busy)
            .help(
                "dev: re-run agent triage for the last \(Self.days) days. Asks first.")
        }
    }
}

// MARK: - small helpers

extension Array {
    /// Bounds-checked read: a clamped-but-stale cursor index must never trap.
    subscript(safe index: Int) -> Element? {
        indices.contains(index) ? self[index] : nil
    }
}

/// A wrapping HStack, for chip rows. SwiftUI has no built-in flow layout.
struct FlowLayout: Layout {
    var spacing: CGFloat = 6

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        let maxWidth = proposal.width ?? .infinity
        var x: CGFloat = 0
        var y: CGFloat = 0
        var rowHeight: CGFloat = 0
        for subview in subviews {
            let size = subview.sizeThatFits(.unspecified)
            if x + size.width > maxWidth, x > 0 {
                x = 0
                y += rowHeight + spacing
                rowHeight = 0
            }
            x += size.width + spacing
            rowHeight = max(rowHeight, size.height)
        }
        return CGSize(width: maxWidth == .infinity ? x : maxWidth, height: y + rowHeight)
    }

    func placeSubviews(
        in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()
    ) {
        var x = bounds.minX
        var y = bounds.minY
        var rowHeight: CGFloat = 0
        for subview in subviews {
            let size = subview.sizeThatFits(.unspecified)
            if x + size.width > bounds.maxX, x > bounds.minX {
                x = bounds.minX
                y += rowHeight + spacing
                rowHeight = 0
            }
            subview.place(at: CGPoint(x: x, y: y), proposal: ProposedViewSize(size))
            x += size.width + spacing
            rowHeight = max(rowHeight, size.height)
        }
    }
}

#if os(macOS)
    // MARK: - the end of a Space walk

    /// THE BOTTOM OF THE LIST. A Space walk that stepped past its last email
    /// lands back here, and this is its reward, in the brand's own light: the
    /// P7 squelch running in a strip across the top of a card, a lock beam
    /// sweeping it once, and a ring and check snapping shut over the trace.
    /// Under it, what the run came to, counting up. Then it leaves on its own
    /// (or on a click).
    ///
    /// Only good news, like the headline above it: what was done, what was
    /// set aside, how quick it was. Never what is left.
    struct WalkComplete: View {
        let summary: AppStore.WalkSummary
        /// The walk emptied the board: the quiet board plays the chime for
        /// that, so this one stays silent rather than doubling it.
        let boardCleared: Bool
        let dismiss: () -> Void

        @Environment(\.accessibilityReduceMotion) private var reduceMotion
        @State private var shown = false
        @State private var beam: CGFloat = -0.3
        @State private var ring: CGFloat = 0
        @State private var check: CGFloat = 0
        @State private var pulse = false
        @State private var counted = false
        @State private var leaving = false

        /// The brand's phosphor blue, the P7 trace's own colour.
        private static let phosphor = Color(hex: 0x82BAF5)
        /// A measure: the card holds a headline and three stats, so it widens
        /// with its type.
        private static var width: CGFloat { Typo.measure(360) }

        private var tally: String {
            var parts: [String] = []
            if summary.done > 0 { parts.append("\(summary.done) done") }
            if summary.skipped > 0 { parts.append("\(summary.skipped) set aside") }
            parts.append(Self.duration(summary.duration))
            return parts.joined(separator: " · ")
        }

        static func duration(_ t: TimeInterval) -> String {
            let s = max(1, Int(t.rounded()))
            if s < 60 { return "\(s)s" }
            return "\(s / 60)m \(s % 60)s"
        }

        var body: some View {
            ZStack {
                Color.black.opacity(shown && !leaving ? 0.45 : 0)
                    .ignoresSafeArea()
                    .onTapGesture { leave() }
                card
                    .scaleEffect(shown && !leaving ? 1 : (reduceMotion ? 1 : 0.88))
                    .opacity(shown && !leaving ? 1 : 0)
            }
            .accessibilityElement(children: .combine)
            .accessibilityLabel("Through the list. \(tally)")
            .task { await run() }
        }

        private var card: some View {
            VStack(spacing: 0) {
                hero
                VStack(spacing: Space.s14) {
                    Text("Through the list.")
                        .font(Typo.hero(30))
                        .foregroundStyle(Palette.ink)
                    HStack(spacing: 0) {
                        stat(counted ? summary.done : 0, "done")
                        divider
                        stat(counted ? summary.skipped : 0, "set aside")
                        divider
                        VStack(spacing: Space.s2) {
                            Text(Self.duration(summary.duration))
                                .font(Typo.serif(22, weight: .medium))
                                .foregroundStyle(Palette.ink)
                                .monospacedDigit()
                            label("time")
                        }
                        .frame(maxWidth: .infinity)
                    }
                }
                // An EDITORIAL card's own inset (see `ModalCard`): roomier
                // than `Space.modal`, one step on the ladder for the sides
                // and foot alike.
                .padding(.horizontal, Space.s24)
                .padding(.top, Space.scaled(18))
                .padding(.bottom, Space.s24)
            }
            .frame(width: Self.width)
            // The strong tint: the stats sit over a busy board, and the
            // board's rows bled through the lighter one.
            .passbandGlass(.pane, cornerRadius: Radius.modal, tint: Palette.glassTintStrong)
            .overlayShadow()
        }

        /// The phosphor strip, the lock beam, and the ring and check over it.
        private var hero: some View {
            ZStack {
                Group {
                    if PhosphorScene.walkDone.isUnavailable {
                        IntroSceneBackdrop(engaged: true, wide: true, paused: false)
                    } else {
                        PhosphorBackdrop(scene: .walkDone)
                    }
                }
                .accessibilityHidden(true)
                // The floor fades into the glass rather than ending at a seam.
                LinearGradient(
                    colors: [.clear, .black.opacity(0.3)], startPoint: .center, endPoint: .bottom)
                // THE LOCK: one bright band swept across the trace, the moment
                // the receiver catches the signal.
                GeometryReader { geo in
                    LinearGradient(
                        colors: [.clear, Self.phosphor.opacity(0.55), .white.opacity(0.85),
                                 Self.phosphor.opacity(0.55), .clear],
                        startPoint: .leading, endPoint: .trailing
                    )
                    .frame(width: geo.size.width * 0.28)
                    .blur(radius: 6)
                    .offset(x: beam * geo.size.width)
                    .blendMode(.plusLighter)
                }
                .allowsHitTesting(false)
                ZStack {
                    if !reduceMotion {
                        ForEach(0..<3) { i in
                            Circle()
                                .stroke(Self.phosphor.opacity(0.7), lineWidth: 1.5)
                                .frame(width: 58, height: 58)
                                .scaleEffect(pulse ? 2.8 : 1)
                                .opacity(pulse ? 0 : 0.9)
                                .animation(
                                    .easeOut(duration: 1.2).delay(Double(i) * 0.2), value: pulse)
                        }
                    }
                    Circle()
                        .fill(.black.opacity(0.35))
                        .frame(width: 58, height: 58)
                    Circle()
                        .trim(from: 0, to: ring)
                        .stroke(Self.phosphor, style: StrokeStyle(lineWidth: 2.5, lineCap: .round))
                        .rotationEffect(.degrees(-90))
                        .frame(width: 58, height: 58)
                        .shadow(color: Self.phosphor.opacity(0.9), radius: 6)
                    CheckShape()
                        .trim(from: 0, to: check)
                        .stroke(
                            .white,
                            style: StrokeStyle(lineWidth: 3, lineCap: .round, lineJoin: .round)
                        )
                        .frame(width: 22, height: 16)
                        .shadow(color: Self.phosphor, radius: 5)
                }
            }
            .frame(height: 140)
            .frame(maxWidth: .infinity)
            // Full bleed at the card's top, so it rounds WITH the card: the
            // card's own radius, not a near miss that leaves glass slivers.
            .clipShape(UnevenRoundedRectangle(
                topLeadingRadius: Radius.modal, topTrailingRadius: Radius.modal))
        }

        private func stat(_ n: Int, _ name: String) -> some View {
            VStack(spacing: Space.s2) {
                Text("\(n)")
                    .font(Typo.serif(22, weight: .medium))
                    .foregroundStyle(Palette.ink)
                    .monospacedDigit()
                    .contentTransition(.numericText(value: Double(n)))
                label(name)
            }
            .frame(maxWidth: .infinity)
        }

        private func label(_ text: String) -> some View {
            Text(text)
                .font(Typo.micro)
                .foregroundStyle(Palette.inkFaint)
                .textCase(.uppercase)
                .tracking(0.5)
        }

        private var divider: some View {
            Rectangle().fill(Palette.hairline).frame(width: 1, height: 28)
        }

        private func run() async {
            if boardCleared {
                NSHapticFeedbackManager.defaultPerformer.perform(.levelChange, performanceTime: .now)
            } else {
                ClearChime.play()
            }
            // Said aloud too: the ring is no reward to someone who cannot see it.
            AccessibilityNotification.Announcement("Through the list. \(tally)").post()
            withAnimation(
                reduceMotion ? .easeOut(duration: 0.2) : .spring(response: 0.42, dampingFraction: 0.66)
            ) { shown = true }
            if reduceMotion {
                ring = 1
                check = 1
                counted = true
            } else {
                try? await Task.sleep(for: .milliseconds(160))
                withAnimation(.easeInOut(duration: 0.55)) { beam = 1.1 }
                try? await Task.sleep(for: .milliseconds(320))
                withAnimation(.easeInOut(duration: 0.4)) { ring = 1 }
                pulse = true
                try? await Task.sleep(for: .milliseconds(300))
                withAnimation(.spring(response: 0.3, dampingFraction: 0.55)) { check = 1 }
                withAnimation(.easeOut(duration: 0.7)) { counted = true }
            }
            try? await Task.sleep(for: .milliseconds(2800))
            // A cancelled task (this card replaced or unmounted) wakes from
            // every sleep at once; it has nothing left to dismiss.
            guard !Task.isCancelled else { return }
            leave()
        }

        private func leave() {
            guard !leaving else { return }
            withAnimation(.easeIn(duration: 0.28)) { leaving = true }
            Task {
                try? await Task.sleep(for: .milliseconds(300))
                dismiss()
            }
        }
    }

    /// A check mark drawn as one stroke, so it can be trimmed on.
    private struct CheckShape: Shape {
        func path(in rect: CGRect) -> Path {
            var p = Path()
            p.move(to: CGPoint(x: rect.minX, y: rect.midY))
            p.addLine(to: CGPoint(x: rect.minX + rect.width * 0.36, y: rect.maxY))
            p.addLine(to: CGPoint(x: rect.maxX, y: rect.minY))
            return p
        }
    }
#endif

// MARK: - zone pills

/// A ZONE'S DOOR, as a pill in its header's corner: flow mode on For your
/// eyes, smart clear on Reading. It is how a key gets found: the reader who
/// clicks one learns, from the keycap on the button they just pressed, that
/// the key was the way all along.
struct ZonePill: View {
    let symbol: String
    let label: String
    /// The key that does the same thing, shown on hover. nil for a door with
    /// no key of its own.
    var key: String?
    let help: String
    let accessibilityName: String
    /// The thing the pill starts is under way and can take a while: the label
    /// changes, a glint drifts round the border, and a press does nothing.
    var working = false
    var workingLabel: String?
    let action: () -> Void

    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var hovering = false

    private var lit: Bool { hovering && !working }
    private var strong: Bool { lit || working }

    var body: some View {
        Button(action: action) {
            HStack(spacing: Space.scaled(5)) {
                // THE KEY ONLY ON HOVER. At rest the corner says what the
                // button is; reaching for it is when the shortcut is worth
                // teaching, so the keycap slides in then and the pill grows
                // out to the left (it is pinned to the card's right edge).
                if lit, let key {
                    Text(key)
                        .font(Typo.mono(9.5, weight: .semibold))
                        .foregroundStyle(Palette.accent)
                        .padding(.horizontal, Space.scaled(9))
                        .padding(.vertical, Space.s2)
                        .background(
                            Capsule(style: .continuous)
                                .fill(Palette.accentSoft)
                        )
                        .overlay(
                            Capsule(style: .continuous)
                                .strokeBorder(Palette.accent.opacity(0.5), lineWidth: 0.75)
                        )
                        // Fades in while the pill widens: the only motion is
                        // the left edge moving out.
                        .transition(.opacity)
                }
                Image(systemName: symbol)
                    .font(Typo.ui(9.5, weight: .semibold))
                    .foregroundStyle(Palette.accent)
                Text(working ? (workingLabel ?? label) : label)
                    .font(Typo.micro)
                    .foregroundStyle(lit ? Palette.ink : Palette.inkDim)
                    .contentTransition(.opacity)
            }
            .padding(.leading, lit && key != nil ? Space.scaled(3) : Space.scaled(9))
            .padding(.trailing, Space.scaled(9))
            .padding(.vertical, Space.scaled(3))
            .background(
                Capsule(style: .continuous)
                    .fill(Palette.accentSoft.opacity(strong ? 0.9 : 0.35))
            )
            #if os(macOS)
                // Alive without a spinner: the flow hint's glint, all the way
                // round the capsule, for as long as the wait lasts.
                .overlay {
                    if working { FlowBorder(radius: 99, moving: !reduceMotion) }
                }
            #endif
            .contentShape(Capsule())
        }
        .buttonStyle(.plain)
        .disabled(working)
        .onHover { hovering = $0 }
        .animation(.easeOut(duration: 0.18), value: hovering)
        .animation(.easeOut(duration: 0.25), value: working)
        .help(help)
        .accessibilityLabel(working ? (workingLabel ?? accessibilityName) : accessibilityName)
    }
}

/// `space` flow mode: the Space walk, as a button in For-your-eyes' corner.
private struct FlowModeButton: View {
    let action: () -> Void

    var body: some View {
        ZonePill(
            symbol: "wind", label: "flow mode", key: "space",
            help: "Walk through For your eyes one email at a time (Space)",
            accessibilityName: "Flow mode", action: action)
    }
}

/// The headline is asked for when the board's key moves, and on a visit.
private struct HeadlineAsk: Equatable {
    var key: String?
    var onScreen: Bool
}
