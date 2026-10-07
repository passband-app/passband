// App shell. Boots settings from the keychain, shows Connect until connected,
// then stacks, bottom to top: rail + routed main view, side panels, the
// fullscreen thread viewer, the action layer (toasts, compose, palettes).
//
// The global 1..5 / ⌘[ / ⌘] / ⌘K bindings register HERE in the "global"
// context, so they compose with whatever context is active rather than being
// gated by it — nav must work even from inside a modal panel.

import SwiftUI

struct RootView: View {
    @Environment(AppStore.self) private var store
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    /// Tester rehearsal only (`--onboarding-rehearsal`): the opening beats
    /// ahead of the practice inbox, with no account loaded underneath.
    @State private var rehearsal = RehearsalSession.shared

    var body: some View {
        @Bindable var store = store

        ZStack {
            if rehearsal.showingIntro {
                OnboardingIntroView(
                    onContinue: { rehearsal.enterMailbox() },
                    continueTitle: rehearsal.entryRequested && !rehearsal.mailboxReady
                        ? "Preparing your guide…" : "Try the practice inbox",
                    continueHint: "Continue to the guided product tour")
                    .id(rehearsal.runID)
                    .task(id: rehearsal.runID) { await rehearsal.prepareMailbox() }
                    // No blur on this side: the intro is a live Metal layer,
                    // and re-blurring it every frame of the flight cost the
                    // compositor a dozen dropped frames. Zoom and fade only.
                    .transition(reduceMotion ? .opacity : .modifier(
                        active: IntroMailboxFlight(scale: 1.22, blur: 0, opacity: 0),
                        identity: IntroMailboxFlight(scale: 1, blur: 0, opacity: 1)))
                    .zIndex(1)
            } else {
                Group {
                    switch store.connStatus {
                    case .loading:
                        LoadingGate()
                    case .connected:
                        if store.tour.blocksMailboxForPractice {
                            LoadingGate()
                        } else {
                            MainShell()
                        }
                    default:
                        ConnectView()
                    }
                }
                .transition(reduceMotion ? .opacity : .modifier(
                    active: IntroMailboxFlight(scale: 0.90, blur: 16, opacity: 0),
                    identity: IntroMailboxFlight(scale: 1, blur: 0, opacity: 1)))
            }
        }
        .animation(reduceMotion ? .easeOut(duration: 0.18) : .timingCurve(0.2, 0.7, 0.2, 1, duration: 0.75),
                   value: rehearsal.showingIntro)
        // THE PRACTICE VEIL. Entering the practice inbox and leaving it both
        // swap the whole read model under the shell; this hides the swap
        // behind one line of copy instead of letting the board flip mid-frame.
        .modifier(PracticeVeil(text: store.tour.veil))
        // THE APP DRAWS FROM THE WINDOW'S TOP EDGE. SwiftUI insets a window's
        // content below the titlebar strip macOS keeps clear for the traffic
        // lights, which left every page carrying an empty band above its own
        // header. Giving the strip back puts each page's TOP BAR up there, level
        // with the buttons; the rail is what makes room for them, starting at
        // `TopBar.height` instead of at the window's edge. Applied once, here,
        // so a surface only has to know the bar's height and not also how the
        // window is inset.
        .ignoresSafeArea(edges: .top)
        // THE NEW-VERSION CARD, above every connection state on purpose: an
        // update is worth taking whether or not the daemon is answering, and
        // the Connect gate is exactly where someone sits when an old client is
        // the reason it will not pair.
        .overlay(alignment: .bottom) {
            UpdateAlert()
                .padding(.bottom, Space.scaled(18))
        }
        // ADD ACCOUNT — the same form as the gate, over a working app. Hung on
        // the whole shell rather than on any one surface: it is raised from
        // Settings, from the rail's account menu, from the Accounts menu, and
        // by a pair link from a second daemon, and none of those can be sure
        // which page is on screen.
        .sheet(isPresented: $store.addAccountSheetOpen) {
            ConnectView(purpose: .addAccount)
        }
        // SHARE PASSBAND, hung here for the same reason Add Account is: it is
        // raised from Settings and from the two-week nudge, and neither knows
        // which page is on screen.
        // WHICH share screen depends on whether there is anything to hand out.
        // `SharePanel` mints a real invite code, and a code is useless until
        // Google's review clears (`ShareGate`), so until then the sheet is the
        // waitlist pointer instead.
        .sheet(isPresented: $store.shareSheetOpen) {
            if ShareGate.invitesEnabled {
                SharePanel()
            } else {
                ShareWaitlistPanel()
            }
        }
        // THE TWO-WEEK ASK. Over the shell rather than inside a page, because
        // it is about the app and not about whatever surface it lands on. It
        // draws nothing until it has earned the right to.
        .overlay { ShareNudgeModal() }
        // WHEN THE ASK FIRES. Hung on the capability rather than on connect,
        // because whether this daemon can share arrives with the first stats
        // and not before; `askIfEarned` is what decides whether a fortnight has
        // actually been used, and it can only ever say yes once.
        .onChange(of: store.shareAvailable) { _, canShare in
            guard !TestFlightNudge.shared.showingNudge, !AgentNudge.shared.showingNudge else { return }
            ShareNudge.shared.askIfEarned(canShare: canShare)
        }
        // THE IPHONE ASK, two days in, for a mailbox no phone has ever
        // connected to. Hung on the daemon's answer for the same reason as the
        // share ask: it arrives with the first stats and not before.
        .overlay { TestFlightNudgeModal() }
        .onChange(of: store.iosConnected) { _, connected in
            TestFlightNudge.shared.askIfEarned(
                iosConnected: connected,
                othersShowing: ShareNudge.shared.showingNudge || AgentNudge.shared.showingNudge)
        }
        // THE AGENT-DOOR AD, a week in, for a mailbox whose door an agent can
        // actually use. Asked at the first sync (ShellWatchers, after the tour
        // and What's New have had their turn) and on every return to the app,
        // below.
        .overlay { AgentNudgeModal() }
        // And on every return to the app, because the answer above changes
        // once per launch at most: a Mac left open across the two-day mark
        // would otherwise never hear the rule again until it was quit.
        .onReceive(NotificationCenter.default.publisher(for: Platform.didBecomeActiveNotification)) { _ in
            TestFlightNudge.shared.askIfEarned(
                iosConnected: store.iosConnected,
                othersShowing: ShareNudge.shared.showingNudge || AgentNudge.shared.showingNudge)
            AgentNudge.shared.askIfEarned(store: store)
        }
        .task {
            // Pay WebKit's process-launch cost at boot rather than on the first
            // email the reader opens.
            EmailWebView.warmProcess()
            if RehearsalMode.includesConnection {
                await rehearsal.restartConnection()
            } else {
                await store.loadSettings()
            }
        }
        .onChange(of: store.accountActionsBlocked) { _, blocked in
            if !blocked { Notifier.shared.drainPendingTap() }
        }
        .onChange(of: store.connStatus) { _, status in
            if status == .connected {
                store.tour.maybeStart()
                guard !store.tour.preparingPractice else { return }
                SitrepPoller.shared.start()
                // EVERY account's ears, not just the live one's: the poller
                // above follows whichever mailbox is on screen, while the event
                // feeds are
                // how the human hears about mail in the ones that are not.
                // NOT while the practice inbox is up: the feeds are the real
                // accounts', and a banner for real mail would point at a thread
                // the practice board cannot open. `exitPractice` comes back
                // through this same transition and starts them then.
                if !RehearsalMode.isEnabled { AccountManager.shared.startAllFeeds() }
                // Warm the emails page at CONNECT, not on its first visit: the
                // list lives in the store, so fetching it now means opening the
                // page lands on rows instead of on "loading mail…". This also
                // warms the head rows' threads, so the first click reads from
                // cache too. The inbox only — the noise page is a place you go.
                Task { await store.refreshMail(.inbox) }
                // Whether this daemon can track opens at all. Needed before the
                // first composer opens, and it decides whether the reader spends
                // any requests looking for receipts.
                Task { await store.refreshTrackingConfig() }
            } else {
                SitrepPoller.shared.stop()
                AccountManager.shared.stopAllFeeds()
                // The Connect gate is now the whole window, and it is the same
                // form the sheet holds. Leaving one stacked on the other would
                // offer two ways to connect at once, the front one adding an
                // account to an install that no longer has any.
                store.addAccountSheetOpen = false
            }
        }
    }
}

/// One continuous flight from the opening slides into the prepared mailbox.
private struct IntroMailboxFlight: ViewModifier {
    let scale: CGFloat
    let blur: CGFloat
    let opacity: Double

    func body(content: Content) -> some View {
        content.scaleEffect(scale).blur(radius: blur).opacity(opacity)
    }
}

/// The shell zoomed back and faded behind one line while the practice inbox
/// is set up or the live one put back. Reduce Motion keeps the fade and drops
/// the zoom and blur.
private struct PracticeVeil: ViewModifier {
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    let text: String?

    func body(content: Content) -> some View {
        let veiled = text != nil
        content
            .scaleEffect(veiled && !reduceMotion ? 0.92 : 1)
            .blur(radius: veiled && !reduceMotion ? 12 : 0)
            .opacity(veiled ? 0 : 1)
            .allowsHitTesting(!veiled)
            .background {
                if let text {
                    VStack(spacing: Space.s14) {
                        ProgressView(text)
                            .foregroundStyle(Palette.inkDim)
                    }
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                    .transition(.opacity)
                }
            }
            .animation(.easeInOut(duration: reduceMotion ? 0.18 : 0.5), value: veiled)
    }
}

private struct LoadingGate: View {
    var body: some View {
        VStack(spacing: Space.s14) {
            Text("passband")
                .font(Typo.serif(34, weight: .medium))
                .foregroundStyle(Palette.ink)
            ProgressView()
                .controlSize(.small)
            SavedConnectionWait()
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}

/// THE SHELL'S STORE WATCHERS, in a view of their own size — which is nothing.
///
/// `onChange(of:)` reads the value it watches, so watching from MainShell's own
/// body made that body a reader of `lastRefresh` — a fresh `Date` written by
/// every poll, on a ten-second clock. The whole shell was then rebuilt every
/// ten seconds: the rail, the routed page, the reader's wrapper chain, the
/// global key bindings. Nothing about it had changed; the app had merely
/// checked its mail. Down here the same watchers invalidate a zero-sized
/// nothing at the same rate, and every action is unchanged.
private struct ShellWatchers: View {
    @Environment(AppStore.self) private var store
    @Environment(Prefs.self) private var prefs

    var body: some View {
        Color.clear
            .frame(width: 0, height: 0)
            .allowsHitTesting(false)
            // DEEPER SEARCH, TURNED OFF, STOPS A LANE THAT IS ALREADY RUNNING
            // (docs/SEARCH.md §6.5). The setting's own line promises that no
            // model reads your mail, so the flip has to be the stopping point:
            // the picker lives on a page the reader walks to, and once they are
            // there the panel they left behind may never see another settled
            // query to notice on.
            //
            // WATCHED FROM THE SHELL, not from the search panel, because the
            // panel is the thing that may not be mounted. Esc closes it on the
            // way to Settings and that only HOLDS the lane (§6.3) — history,
            // cards and all — so a watcher living inside it would be gone at
            // exactly the moment it was needed.
            .onChange(of: prefs.deeperSearch) { _, choice in
                let move = DeeperSearchPolicy.preferenceChanged(
                    to: choice, laneStarted: store.search.laneStarted)
                // The verdict stays: the words in the bar are still the same
                // question, so flipping back to automatic can offer to run it
                // again without the reader retyping anything.
                if move == .stop { store.resetSearchLane(keepingVerdict: true) }
            }
            // Connection starts onboarding before the shell is revealed.
            // Refreshes can still resume a pending live summary.
            .onChange(of: store.lastRefresh) { old, new in
                if old == nil, new != nil {
                    store.tour.maybeStart()
                    // Onboarding claims the moment before release notes.
                    store.whatsNew.maybeShow()
                    // The agent-door ad waits out both: it checks for them.
                    AgentNudge.shared.askIfEarned(store: store)
                }
            }
    }
}

struct MainShell: View {
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    /// The readers on screen: the open thread, plus the one it is turning
    /// away from while an S is in flight (behind it, so it is drawn first).
    /// An undone S turns the other way, with the outgoing card on top.
    private func readerSlots(_ threadId: String) -> [String] {
        guard let turn = store.skipTurn, turn.outgoing != threadId else { return [threadId] }
        return turn.reversed ? [threadId, turn.outgoing] : [turn.outgoing, threadId]
    }

    @Environment(AppStore.self) private var store
    @Namespace private var shellGlass

    var body: some View {
        @Bindable var store = store

        // The compose pane takes HALF the window — the routed page shrinks
        // beside it rather than disappearing under a modal. GeometryReader is
        // the window's own measure; the floor keeps the pane usable when the
        // window itself is narrow.
        GeometryReader { geo in
            let composeWidth = max(420, geo.size.width / 2)
            // How much of the page's width is spoken for by a surface beside
            // it, named only so its change can be told NOT to animate — see
            // the `.animation(nil, value:)` on the page and on the reader.
            let pageInset: CGFloat = store.composeOpen && !store.composeExpanded ? composeWidth : 0
            let readerInset: CGFloat = (store.sideView.isOpen ? sidePanelWidth : 0) + pageInset

        ZStack(alignment: .topLeading) {
            // THE QUIET BOARD: the sitrep with nothing on it sits on the live
            // P7 squelch. Drawn HERE, under the rail and the traffic lights,
            // so it fills the window rather than the page; and OUTSIDE the
            // layer below, because that layer blurs under a modal and a
            // blurred live Metal layer stutters. Not while an email is open:
            // the sitrep stays mounted under the reader, and a scene nobody
            // can see would keep rendering behind it.
            if QuietBoard.shared.isQuiet && store.threadId == nil {
                ClearScene()
                    .ignoresSafeArea()
                    .transition(.opacity)
            }
            // EVERYTHING A MODAL SITS ON TOP OF, blurred as one layer while a
            // modal is up. Blurring is what keeps the app visible behind ⌘K:
            // layout and colour survive a defocus, a material scrim paints
            // over them.
            ZStack(alignment: .topLeading) {
            HStack(spacing: 0) {
                SidebarRail(namespace: shellGlass)
                VStack(spacing: 0) {
                    ConnectionBanner()
                    // Daemon down with nothing ever loaded: empty bands would
                    // read as an empty inbox. Settings stays reachable so the
                    // token/URL can be fixed.
                    if store.daemonDown && store.activeView != .settings {
                        DaemonDownPane()
                    } else {
                        routedBody
                    }
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                // DISCOVER COMES OUT OF THE RAIL: it slides in from the left,
                // where its ring is, and back the same way on Esc. Keyed on
                // the one bool, so no other page change is animated; clipped,
                // so the page arrives from under the rail, not across it.
                .clipped()
                .animation(
                    reduceMotion ? nil : .smooth(duration: 0.34), value: store.activeView == .discovery)
                // INVISIBLE UNDER A SETTLED READER, and still mounted. The
                // reader is opaque and covers the page, but a covered page is
                // still composited — six glass cards re-sampling whatever is
                // under them, on every frame the reader is up and every
                // frame of a done+next resolve re-rendering the board beneath
                // it. Opacity keeps the page's state, layout and geometry
                // reports (the tour measures it) and drops only the drawing.
                // Visible again the moment the reader is in flight, so the
                // board is there under a departing email.
                .opacity(
                    (store.threadId != nil && store.threadFlight == .settled)
                        || store.scheduledPreviewRow != nil ? 0 : 1)
                // The composer's STRIP: a working surface in the layout, NOT an
                // overlay — no scrim, no blur, the page beside it shrinks and
                // stays live. The pane itself is drawn in its own layer below,
                // so that opening it out to full screen moves ONE view rather
                // than swapping in a second one (which would drop the caret,
                // the focus and the editor's undo stack on every toggle).
                if store.composeOpen && !store.composeExpanded {
                    Color.clear
                        .frame(width: composeWidth)
                        .allowsHitTesting(false)
                }
            }
            // THE STRIP IS RESERVED IN ONE STEP, NOT SLID INTO. The pane slides
            // (its own transition below); the page beside it must not shrink
            // along with it, because the page is a live layout — the board
            // re-judges its rail breakpoint at every width it passes through
            // and rebuilds every zone when it crosses, and a reader's web
            // frames reflow their documents on every one of the thirteen
            // frames. One reflow at the start, then the pane arrives over the
            // space it left. Innermost animation wins for the values it names.
            .animation(nil, value: pageInset)

            // Side panel surfaces (browse / search).
            if store.sideView.isOpen {
                SidePanel()
                    .transition(.move(edge: .trailing).combined(with: .opacity))
                    .zIndex(10)
            }

            // Email viewer — above the side panels, below the action layer so
            // compose/undo/palette stay on top. It fills the window EXCEPT an
            // open side panel's strip: opening a hit from search must leave the
            // results visible beside the email. Esc sheds the panel first (the
            // email stays), a second Esc closes the reader.
            //
            // THE SAME SLOT holds a scheduled mail's read-only preview
            // (ScheduledPreview): opened from Sent where a sent row's thread
            // would open, with the same rail, insets and layer. The two never
            // share it (`openThread` and `openScheduledPreview` each clear
            // the other).
            let preview = store.threadId == nil ? store.scheduledPreviewRow : nil
            if store.threadId != nil || preview != nil {
                HStack(spacing: 0) {
                    // THE RAIL STAYS: the reader insets past it instead of
                    // covering it, so 1..5 stay clickable while you read. The
                    // TITLE STRIP above it is the one exception: it takes the
                    // reader's own backdrop, so the top bar reads as one bar
                    // in the reader's colour instead of seaming where the
                    // page's ground meets the reader's at the rail's edge.
                    VStack(spacing: 0) {
                        // Not in fullscreen: the rail itself runs to the true
                        // top there (no traffic lights, no strip), and this
                        // would paint the reader's ground over it.
                        if !WindowState.shared.isFullscreen {
                            ReaderBackdrop()
                                .frame(height: TopBar.height)
                        }
                        Color.clear
                    }
                    .frame(width: SidebarRail.railWidth)
                    .allowsHitTesting(false)
                    // THE READER'S FLIGHT, and it is a plain offset: the email
                    // you finished is lifted out through the top, the next one
                    // is put one window away and walked in. ThreadViewer moves
                    // the state; this is only where it lands. A ZStack so the
                    // offsets have somewhere to move WITHIN, and a clip so a
                    // reader in flight never paints over the rail beside it.
                    ZStack {
                        // A Reading stack flies its own card under the header
                        // (see ThreadViewer.deck), so the window holds still.
                        let flight: AppStore.ThreadFlight =
                            store.readingStack ? .settled : store.threadFlight
                        // OFFSET AND OPACITY ONLY — no scale. The reader is a
                        // column of live WKWebViews, and a transform on a
                        // platform view is re-rasterised every frame it
                        // changes; the departure ran on the same main thread
                        // it was starving.
                        if let threadId = store.threadId {
                            // THE GROUND THE EMAILS FLY OVER, and it does not
                            // fly. Between one email leaving and the next
                            // walking in there is a frame with no reader in
                            // the window, and without this the sitrep under
                            // it flashed through on every E and S. Only while
                            // the flight CONTINUES: an exit that closes the
                            // reader should reveal the sitrep as it goes.
                            if store.skipTurn != nil || store.flightContinues {
                                ReaderBackdrop()
                                    .ignoresSafeArea()
                            }
                            // ONE READER, OR TWO FOR THE LENGTH OF AN S. A
                            // ForEach keyed by thread id rather than an `.id`,
                            // so the outgoing reader keeps its identity (and
                            // its loaded mail) through the turn instead of
                            // being rebuilt.
                            ForEach(readerSlots(threadId), id: \.self) { id in
                                let turn = store.skipTurn
                                let outgoing = turn?.outgoing == id
                                ThreadViewer(
                                    threadId: id,
                                    turn: turn.map { t in
                                        // The card underneath recedes and dims;
                                        // the one on top slides the full width.
                                        // Forward, the top card is arriving;
                                        // reversed, it is the one leaving.
                                        let under = t.reversed ? 1 - t.progress : t.progress
                                        let raised = outgoing == t.reversed
                                        return raised
                                            ? .init(
                                                cardSlide: t.reversed ? t.progress : 1 - t.progress,
                                                cardOpacity: 1,
                                                headerOpacity: outgoing
                                                    ? t.outgoingHeader : t.incomingHeader,
                                                incoming: !outgoing,
                                                raised: true)
                                            : .init(
                                                cardSlide: -0.3 * under,
                                                cardOpacity: 1 - 0.55 * under,
                                                headerOpacity: outgoing
                                                    ? t.outgoingHeader : t.incomingHeader,
                                                incoming: !outgoing,
                                                raised: false)
                                    })
                                .offset(outgoing ? .zero : flight.offset(in: geo.size))
                                .opacity(outgoing ? 1 : flight.opacity)
                                .allowsHitTesting(!outgoing)
                            }
                        } else if let preview {
                            // No flight: a preview is opened and closed, never
                            // walked through.
                            ScheduledPreview(row: preview)
                                .id(preview.id)
                        }
                    }
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                    .clipped()
                    // THE SPACE WALK'S HINT, outside the flight: the emails fly
                    // under it and it holds still, which is what lets it read
                    // as part of the window rather than part of each email.
                    .overlay(alignment: .bottomTrailing) {
                        if store.walk != nil, store.threadId != nil {
                            // S works on every email of a walk but a walk of
                            // one, where there is nothing to skip to or past.
                            WalkHint(canSkip: store.threadQueue.count > 1)
                                .padding(.bottom, Space.scaled(18))
                                .padding(.trailing, Space.s24)
                                .transition(.opacity)
                        }
                    }
                    .animation(.easeInOut(duration: 0.2), value: store.walk != nil)

                    if store.sideView.isOpen {
                        // Reserves the strip without taking clicks off the panel
                        // sitting under it.
                        Color.clear
                            .frame(width: sidePanelWidth)
                            .allowsHitTesting(false)
                    }
                    // Same reservation for the compose pane: `c` from the reader
                    // opens it in the layout BELOW this layer, and the reader
                    // insetting past it is what keeps it visible.
                    if store.composeOpen && !store.composeExpanded {
                        Color.clear
                            .frame(width: composeWidth)
                            .allowsHitTesting(false)
                    }
                }
                // Same as the page above: the reader steps aside once, and the
                // pane slides in over the space. Animating the inset made every
                // web frame in the thread reflow per frame, which is the
                // flicker `f` used to show.
                .animation(nil, value: readerInset)
                // NO TRANSITION. Opening an email is a jump, not a dissolve: a
                // crossfade shows two surfaces at once on a surface the reader
                // enters and leaves constantly, which reads as lag.
                .zIndex(20)
            }

            // THE COMPOSER, in both of its sizes. Side-by-side it sits in the
            // strip reserved above, UNDER the side panels and the reader (which
            // insets past it), exactly where it sat when it lived in the
            // layout. Opened out it takes everything right of the rail, over
            // the page and an open reader alike, with the mail in a centred
            // column (ComposePane holds the measure). The rail stays, as it
            // does for the reader. Same view in both: only its frame and its
            // layer change.
            if store.composeOpen {
                HStack(spacing: 0) {
                    if store.composeExpanded {
                        Color.clear
                            .frame(width: SidebarRail.railWidth)
                            .allowsHitTesting(false)
                    } else {
                        Spacer(minLength: 0)
                    }
                    ComposePane()
                        .frame(width: store.composeExpanded ? nil : composeWidth)
                }
                .transition(.move(edge: .trailing).combined(with: .opacity))
                .zIndex(store.composeExpanded ? 25 : 5)
            }
            }
            .blur(radius: store.modalOverlayOpen ? 9 : 0)


            // Global overlays: undo toasts, compose ceremony, palettes. OUTSIDE
            // the blurred stack — the modal itself must stay sharp.
            ActionLayer()
                .zIndex(30)
        }
        // The tour measures dashboard regions and places its coach marks in
        // ONE space, and this ZStack is the closest common ancestor of the
        // sitrep that reports them and the action layer that draws on them.
        .coordinateSpace(.named(tourSpace))
        .animation(.easeOut(duration: 0.18), value: store.modalOverlayOpen)
        // Mail landing on a quiet board fades the scene out, never cuts it.
        .animation(.easeInOut(duration: 0.9), value: QuietBoard.shared.isQuiet)
        .animation(.smooth(duration: 0.22), value: store.sideView)
        // The BOOL, never the ComposeState: the state changes on every
        // keystroke, and animating that would smear typing.
        .animation(.smooth(duration: 0.22), value: store.composeOpen)
        .animation(.smooth(duration: 0.22), value: store.composeExpanded)
        // threadId is deliberately NOT animated HERE — opening a thread is a
        // jump. The one motion it has is the done+next flight, and that one is
        // animated at the call site, where the direction is known.
        .keyBindings(.global, globalBindings)
        // The two store watchers are a LEAF of their own (see ShellWatchers) —
        // watching from here made this body a reader of the poll clock.
        .background { ShellWatchers() }
        }
    }

    @ViewBuilder
    private var routedBody: some View {
        switch store.activeView {
        case .sitrep: SitrepView()
        case .emails: EmailsView()
        case .auth: RoutedHost(view: .auth) { AuthView() }
        case .rules: RoutedHost(view: .rules) { RulesView() }
        case .groups: RoutedHost(view: .groups) { GroupsView() }
        case .usage: UsageView()
        case .settings: SettingsView()
        case .process: RoutedHost(view: .process) { ProcessView() }
        case .discovery:
            DiscoveryView()
                .transition(reduceMotion ? .opacity : .move(edge: .leading).combined(with: .opacity))
        }
    }

    /// 1..5 view nav + ⌘[ back / ⌘] forward + ⌘K ask bar. The ⌘ chords use the
    /// `meta` flag so a bare "[" / "]" never triggers them; `allowInInput` keeps
    /// them working with a search/compose field focused, since a chord is not a
    /// typed character.
    private var globalBindings: [KeyBinding] {
        // THE ONE MODAL THAT REALLY IS MODAL. Every other overlay COMPOSES with
        // this set deliberately (see KeyDispatch's header: "1..5 view nav and ⌘K
        // keep firing from inside a modal") — which is right for a palette you
        // are meant to act from, and wrong for a re-triage: the board behind the
        // scrim is mid-rewrite, so navigating to it, searching it, or undoing
        // into it are all reads and writes against verdicts that are being
        // replaced. Refusing the whole set HERE is the only place that covers
        // every one of them at once.
        guard store.retriage == nil else { return [] }
        if RehearsalMode.isEnabled {
            return [
                KeyBinding(declining: "u", "undo last action") {
                    guard !store.undos.isEmpty else { return false }
                    Task { await store.fireUndo() }
                    return true
                },
                KeyBinding("\\", "toggle light/dark theme") { Prefs.shared.flipTheme() },
            ]
        }
        var bindings: [KeyBinding] = MainView.mainViews.enumerated().map { index, view in
            KeyBinding("\(index + 1)", "go to \(view.rawValue)") { store.setView(view) }
        }
        bindings.append(
            KeyBinding("[", "back", meta: true, allowInInput: true) { store.goBack() })
        bindings.append(
            KeyBinding("]", "forward", meta: true, allowInInput: true) { store.goForward() })
        bindings.append(
            KeyBinding("k", "ask your inbox", meta: true, allowInInput: true) {
                store.askBarOpen = true
            })
        // ⌘⇧K starts over. Bound as the CAPITAL letter, which is how the
        // registry spells shift for a letter key, and matched by the exact pass
        // ahead of the lowercase ⌘K above.
        bindings.append(
            KeyBinding("K", "new ask chat", meta: true, allowInInput: true) {
                store.newAskChat()
            })
        // Global, not list-only: `/`, `\` and `?` must work on every surface,
        // including the sitrep the app lands on. All three stay input-guarded
        // (no allowInInput), so typing them into a field still types the
        // character.
        bindings.append(
            KeyBinding("/", "search") { store.openSearch() })
        // Undo is global because the toasts are: an action fired from any
        // surface parks its undo in the ActionLayer, which advertises `u`
        // everywhere. Declining, so with nothing pending the key stays free
        // for surface bindings (the thread viewer's `u` = unsubscribe).
        bindings.append(
            KeyBinding(declining: "u", "undo last action") {
                guard !store.undos.isEmpty else { return false }
                Task { await store.fireUndo() }
                return true
            })
        bindings.append(
            KeyBinding("\\", "toggle light/dark theme") { Prefs.shared.flipTheme() })
        bindings.append(
            KeyBinding("?", "keyboard shortcuts") { store.shortcutsOpen.toggle() })
        return bindings
    }
}

/// Host for the full main views behind the rail: Auth / Rules / Groups.
///
/// These inner views register their list-style keys into the "modal" KeyContext
/// and never push a context themselves, so this host pushes it while mounted.
/// Only one routed view is mounted at a time, so there is never a competing
/// "list" set active; the global 1..5 keys still fire because "global"
/// composes with "modal".
struct RoutedHost<Content: View>: View {
    @Environment(AppStore.self) private var store
    let view: MainView
    @ViewBuilder var content: Content

    /// Page name, and the line beside it that says what the page IS.
    ///
    /// Two values rather than one string with a dash in it: the subtitle is set
    /// in the sitrep masthead's own small caps, so it has to reach the header as
    /// its own text. (It read "Rules — sender rules", which also put an em dash
    /// in user-facing copy, which this app does not do.)
    private var heading: (title: String, subtitle: String?) {
        switch view {
        case .rules: ("Rules", "sender rules")
        case .groups: ("Groups", "named audiences")
        default: (view.label, nil)
        }
    }

    var body: some View {
        Group {
            if view == .auth {
                // Auth owns its entire surface (own header band, two-column
                // body), so it opts out of the shared chrome.
                content
            } else {
                VStack(alignment: .leading, spacing: 0) {
                    RoutedHeader(title: heading.title, subtitle: heading.subtitle)
                    content
                }
            }
        }
        .keyContext(.modal)
        // Esc = back to the sitrep. Overlays these views open push their OWN
        // contexts above this one, so their Esc wins while they're up and this
        // fires only from the bare list.
        .keyBindings(.modal, [
            KeyBinding("Escape", "back to sitrep") { store.setView(.sitrep) }
        ])
    }
}

/// The shared page header for routed views.
struct RoutedHeader<Trailing: View>: View {
    let title: String
    /// The page's own description, set in the SITREP MASTHEAD'S small caps —
    /// the same pairing as `passband` + `SITREP`, so a routed page and the
    /// dashboard read as one app rather than as two typographic ideas.
    var subtitle: String?
    @ViewBuilder var trailing: Trailing

    init(title: String, subtitle: String? = nil, @ViewBuilder trailing: () -> Trailing) {
        self.title = title
        self.subtitle = subtitle
        self.trailing = trailing()
    }

    var body: some View {
        // The OUTER stack stays centre-aligned: `trailing` carries buttons and
        // status dots, and baseline-aligning those to a text run drops them off
        // the bar's centre line. Only the title pair is baseline-aligned, which
        // is what sits the small caps on the title's foot.
        HStack(spacing: Space.s10) {
            HStack(alignment: .firstTextBaseline, spacing: Space.scaled(7)) {
                Text(title)
                    .font(Typo.ui(15, weight: .semibold))
                    .foregroundStyle(Palette.ink)
                if let subtitle {
                    Text(subtitle)
                        .font(Typo.micro)
                        .foregroundStyle(Palette.inkFaintest)
                        .textCase(.uppercase)
                }
            }
            Spacer(minLength: Space.s8)
            trailing
        }
        .padding(.horizontal, Space.scaled(22))
        // THE BAR'S HEIGHT, not this header's own padding: it sits in the window
        // strip beside the traffic lights, and the rail's top edge is cut to the
        // same line. A header that set its own height would break that join.
        .frame(height: TopBar.height)
        .frame(maxWidth: .infinity, alignment: .leading)
        .overlay(alignment: .bottom) { Hairline() }
    }
}

extension RoutedHeader where Trailing == EmptyView {
    init(title: String, subtitle: String? = nil) {
        self.init(title: title, subtitle: subtitle) { EmptyView() }
    }
}

// MARK: - how the reader changes threads

extension AppStore.ThreadFlight {
    /// WHERE THE READER SITS for each beat of the flight, a full window away in
    /// whichever direction it is travelling — far enough that the email is
    /// genuinely off the screen rather than peeking in at an edge.
    func offset(in window: CGSize) -> CGSize {
        switch self {
        case .settled: .zero
        case .departing: CGSize(width: 0, height: -window.height)
        case .departingDown: CGSize(width: 0, height: window.height)
        case .departingTrailing: CGSize(width: window.width, height: 0)
        case .entering(.bottom): CGSize(width: 0, height: window.height)
        case .entering(.trailing): CGSize(width: window.width, height: 0)
        case .entering(.top): CGSize(width: 0, height: -window.height)
        }
    }

    /// Only the departure fades, and slightly: enough for the email to read as
    /// leaving the reader's plane rather than merely sliding. What arrives
    /// arrives whole. (It used to shrink to 0.96 as well; a scale on a column
    /// of web frames re-rasterises every one of them per frame, so it went.)
    var opacity: Double { isDeparting ? 0 : 1 }

    /// Both exits, whichever way they leave. The next variant added is a
    /// departure or an arrival, and it says so here once.
    var isDeparting: Bool {
        switch self {
        case .departing, .departingDown, .departingTrailing: true
        case .settled, .entering: false
        }
    }
}
