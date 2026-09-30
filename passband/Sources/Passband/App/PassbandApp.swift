// App entry point. Opens ONE window whose background is the native AppKit
// backdrop — that is the layer every `.glassEffect` in the app refracts.

import AppKit
import SwiftUI

@main
struct PassbandApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var delegate
    @State private var store = AppStore.shared
    @State private var prefs = Prefs.shared
    @State private var accounts = AccountManager.shared

    init() {
        Typo.registerBundledFonts()
    }

    var body: some Scene {
        Window("Passband", id: "main") {
            ZoomedWindowContent(zoom: prefs.zoom) {
                RootView()
                    .environment(store)
                    .environment(prefs)
            }
                .preferredColorScheme(prefs.theme.colorScheme)
                // The layout's floor, in window points: a zoomed shell needs a
                // proportionally bigger window to hold the same page.
                .frame(minWidth: Zoom.minLogicalSize.width * prefs.zoom,
                       minHeight: Zoom.minLogicalSize.height * prefs.zoom)
                // The traffic lights sit on the top bar's centre line, and the
                // bar's height on screen is a zoomed one.
                .onChange(of: prefs.zoom) { WindowConfigurator.realignTrafficLights() }
                .background(WindowBackdrop().ignoresSafeArea())
                .background(WindowConfigurator())
                .onAppear {
                    KeyMonitor.shared.onZoomIn = { Zoom.zoomIn() }
                    KeyMonitor.shared.install()
                    // The tester's panel beside the window, never for a
                    // customer: the flag is a launch argument only a
                    // rehearsal script passes.
                    if RehearsalMode.launchedStandalone { RehearsalControlPanel.shared.show() }
                }
                // passband://pair links. Parked on the store rather than acted
                // on here: only the Connect gate can pair, and an install that
                // already has an identity must not re-pair over it.
                .onOpenURL { store.receivePairLink($0) }
        }
        .defaultSize(width: 1320, height: 880)
        .windowStyle(.hiddenTitleBar)
        .windowToolbarStyle(.unifiedCompact)
        .commands {
            PassbandCommands(store: store, prefs: prefs, accounts: accounts)
            if RehearsalMode.launchedStandalone {
                CommandMenu("Rehearsal") {
                    Button("Show rehearsal controls") { RehearsalControlPanel.shared.show() }
                }
            }
        }
    }
}

final class AppDelegate: NSObject, NSApplicationDelegate {
    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApp.setActivationPolicy(.regular)
        NSApp.activate(ignoringOtherApps: true)
        // Install before launch finishes: a notification tapped while Passband
        // was not running is delivered the moment a delegate exists, and a
        // center without one drops it.
        Notifier.shared.install()
        // Attachment bytes staged for Quick Look are torn down when the panel
        // closes; this is the sweep for the ones a crash or a hard quit stranded.
        StagedAttachment.purgeRoot()
        // Same rule, same reason: a banner's sender tile is written to disk for
        // the system to carry off, and this clears the ones it never took.
        NotificationIcon.purgeRoot()
        Analytics.start()
        // The boot view never passes through route(to:), so its screen event
        // is recorded here or not at all.
        Analytics.screen(AppStore.shared.activeView.rawValue)
    }

    /// ⌘Q INSIDE A SEND HOLD. TEMPORARY: this goes away once the send hold
    /// moves into the daemon. Until then a held send lives only in memory for
    /// its five seconds, so it is parked as a draft through the account
    /// switch's own `parkHeldSends` and quit waits for that save, BOUNDED so an
    /// unreachable daemon cannot wedge quit. Sparkle's install path ends in
    /// `NSApp.terminate` and simply waits here too.
    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        MainActor.assumeIsolated {
            let store = AppStore.shared
            // An OPEN composer that was typed into gets its last save too:
            // `flush` is a no-op for an untouched one, so a quit with nothing
            // new to keep still terminates at once. Both are flushed before
            // the test, so neither is skipped by a short-circuit.
            let pane = DraftSaver.shared.flush(.compose, store.compose)
            let inline = DraftSaver.shared.flush(.inlineReply, store.inlineReply)
            let held = store.hasHeldSends
            // A schedule POST, pull-back DELETE or undo re-POST on the wire:
            // its answer can reopen the mail in a composer, which is then the
            // only copy. Quit waits for it, then flushes that composer.
            let onWire = store.outboxOnWire > 0
            guard held || pane || inline || onWire else { return .terminateNow }
            if held { store.parkHeldSends(quitting: true) }
            // Before the first stats land, every save queued above first waits
            // up to `verdictWait` to learn which route to write by; a bound
            // shorter than that kills the process before the write it waited
            // for. Once stats are in, the write goes out at once, but a parked
            // send and an edited pane composer share slot `.compose`, whose
            // writes go one after the other: two round trips, and the parked
            // one (never toasted, so nobody knows to look) is the one a short
            // bound cuts on a slow daemon.
            let limit: Duration =
                store.stats == nil
                ? DraftSaver.verdictWait + .seconds(2) : held || onWire ? .seconds(8) : .seconds(3)
            Task { @MainActor in
                // Whichever lands first answers; a reply of `true` ends the
                // process, so the loser never gets to answer twice.
                let bound = Task { @MainActor in
                    try? await Task.sleep(for: limit)
                    if !Task.isCancelled { NSApp.reply(toApplicationShouldTerminate: true) }
                }
                await DraftSaver.shared.settle()
                if onWire {
                    await store.awaitOutboxOnWire()
                    // No-ops unless the answer put the mail in a composer
                    // (`putBack` and `openPulledBack` arm its save).
                    DraftSaver.shared.flush(.compose, store.compose)
                    DraftSaver.shared.flush(.inlineReply, store.inlineReply)
                    await DraftSaver.shared.settle()
                }
                bound.cancel()
                NSApp.reply(toApplicationShouldTerminate: true)
            }
            return .terminateLater
        }
    }

    // FALSE, because the app is a notifier: terminating on last-window-close
    // would drop the event stream, so the notification the window was closed to
    // wait for would never arrive. ⌘Q and the app menu still quit.
    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { false }

    /// Dock click with nothing on screen: put the window back.
    func applicationShouldHandleReopen(
        _ sender: NSApplication, hasVisibleWindows flag: Bool
    ) -> Bool {
        if !flag { MainWindow.show() }
        return true
    }
}

/// Native menu equivalents for the app's global chords. These duplicate
/// registry bindings on purpose: the registry owns dispatch semantics (its
/// layering and input guard), the menu owns discoverability.
struct PassbandCommands: Commands {
    let store: AppStore
    let prefs: Prefs
    let accounts: AccountManager
    @ObservedObject private var updater = Updater.shared

    /// ⌘number SWITCHES ACCOUNTS. One constant, because the decision behind it
    /// is one decision: the digits used to route the Go menu's five views, and
    /// they were taken away from it (the bare `1`–`5` in RootView's global
    /// bindings still route, input-guarded, which is the whole reason the menu
    /// could give the chord up). A menu shortcut fires app-wide — inside text
    /// fields, inside modals — and for "show me my other mailbox" that is
    /// correct: it is not a character anyone means to type.
    private static let accountChordModifiers: EventModifiers = [.command]

    /// How many accounts get a chord. Nine digits, and ⌘0 is not a tenth.
    private static let chordedAccounts = 9

    /// The chord for the account at one position, or none past the ninth —
    /// those switch from the menu (or by cycling), which is the honest whole of
    /// what a tenth account can have.
    private static func accountChord(_ index: Int) -> KeyboardShortcut? {
        guard index < chordedAccounts else { return nil }
        return KeyboardShortcut(
            KeyEquivalent(Character("\(index + 1)")), modifiers: accountChordModifiers)
    }

    /// A dev re-triage owns the screen (see `RetriageModal`). These commands are
    /// AppKit's, not `KeyDispatch`'s, so refusing the global keymap does not
    /// reach them — ⌘1 would still switch accounts out from under a run that is
    /// counting the mailbox it started on, and ⌘K would still open the ask bar.
    /// Each one that navigates or writes is disabled for the duration.
    /// Also the whole of the practice inbox and its veils
    /// (`accountActionsBlocked`), ON PURPOSE for every command here and not
    /// only the account ones: navigation is refused by `setView` for the
    /// duration, a composer could only fail to send, the ask bar has no relay
    /// behind it, and a refresh has nothing to refresh. A menu that greys out
    /// is honest about that; one that silently does nothing is not.
    private var blocked: Bool { store.retriage != nil || store.accountActionsBlocked }

    var body: some Commands {
        // Directly under "About Passband", where every Mac app keeps it.
        CommandGroup(after: .appInfo) {
            Button("Check for Updates…") { updater.check() }
                .disabled(!updater.canCheck)
        }

        // ⌘N composes from ANYWHERE, including the surfaces where the bare `c` is
        // deliberately out of reach (inside a modal, or with a text field focused).
        CommandGroup(replacing: .newItem) {
            Button("New Message") { store.openComposeNew() }
                .keyboardShortcut("n")
                .disabled(blocked)
        }

        // Settings is a routed view rather than a separate window, so the
        // standard ⌘, routes instead of opening one.
        CommandGroup(replacing: .appSettings) {
            Button("Settings…") { store.setView(.settings) }
                .keyboardShortcut(",", modifiers: .command)
                .disabled(blocked)
        }

        CommandMenu("Go") {
            // NO KEYBOARD SHORTCUT. These five used to hold ⌘1–⌘5; the digits
            // went to account switching, and the BARE `1`–`5` in
            // RootView.globalBindings do the routing — which they always did,
            // behind the dispatcher's input guard, so nothing was lost but the
            // menu's ability to advertise them.
            ForEach(Array(MainView.mainViews.enumerated()), id: \.element) { _, view in
                Button(view.label) { store.setView(view) }
                    .disabled(blocked)
            }
            Divider()
            Button("Usage") { store.setView(.usage) }
                .disabled(blocked)
            Button("Settings") { store.setView(.settings) }
                .disabled(blocked)
            Divider()
            Button("Back") { store.goBack() }
                .keyboardShortcut("[", modifiers: [.command])
                .disabled(blocked || !store.canGoBack)
            Button("Forward") { store.goForward() }
                .keyboardShortcut("]", modifiers: [.command])
                .disabled(blocked || !store.canGoForward)
        }

        // ACCOUNTS — one entry per mailbox this install knows about, on the
        // digits the Go menu gave up. Present even with a single account: it is
        // where "Add Account…" lives, and a menu that appears only once you
        // already have two is a feature nobody finds.
        CommandMenu("Accounts") {
            ForEach(Array(accounts.accounts.enumerated()), id: \.element.id) { index, account in
                Button {
                    Task { await accounts.switchTo(account.id) }
                } label: {
                    // The live one wears a check, the way every account picker
                    // on this platform says which one you are looking at.
                    if account.id == accounts.activeId {
                        Label(account.displayName, systemImage: "checkmark")
                    } else {
                        Text(account.displayName)
                    }
                }
                .keyboardShortcut(Self.accountChord(index))
                // Nothing to switch INTO from the Connect gate: the app has no
                // identity on screen, and pointing the client at a daemon
                // behind a screen that is still asking for one is a switch the
                // human cannot see happen. Nor OUT of a live re-triage, which
                // is counting the mailbox it started on — the run's own epoch
                // fence catches the switch, but the switch should not be
                // offered in the first place.
                .disabled(blocked || store.connStatus != .connected)
            }
            Divider()
            // DELIBERATELY CHORDLESS. ⌘` is the system's window cycler, and
            // every account inside the first nine already has a digit of its
            // own — this is the tenth-account escape hatch and a place for the
            // menu to say the list wraps, not a primary way to move.
            Button("Next Account") { Task { await accounts.switchToNext() } }
                .disabled(
                    blocked || store.connStatus != .connected || accounts.accounts.count < 2)
            Divider()
            Button("Add Account…") { store.addAccountSheetOpen = true }
                // The gate is already a connect form. Offering a second one on
                // top of it would add an account to an install that has none.
                .disabled(blocked || store.connStatus != .connected)
        }

        CommandMenu("Inbox") {
            Button("Ask Your Inbox…") { store.askBarOpen = true }
                .keyboardShortcut("k", modifiers: [.command])
                .disabled(blocked)
            Button("New Ask Chat") { store.newAskChat() }
            .keyboardShortcut("k", modifiers: [.command, .shift])
            .disabled(blocked)
            Button("Search…") { store.openSearch() }
                .keyboardShortcut("f", modifiers: [.command])
                .disabled(blocked)
            Divider()
            Button("Check for New Mail") {
                Task { await SitrepPoller.shared.triggerMailRefresh() }
            }
            .keyboardShortcut("r", modifiers: [.command])
            .disabled(blocked)
            Divider()
            Button("Undo Last Action") { Task { await store.fireUndo() } }
                .keyboardShortcut("z", modifiers: [.command])
                .disabled(blocked || store.undos.isEmpty)
        }

        CommandGroup(after: .toolbar) {
            // Live everywhere, including a re-triage run: it changes how the
            // screen is drawn, never what is on it.
            // ⌘= is the key under "+", and the chord most people press for
            // zoom in. ⌘+ proper (⇧=) is caught in KeyMonitor, since a menu
            // item has room for one chord.
            Button("Zoom In") { Zoom.zoomIn() }
                .keyboardShortcut("=", modifiers: [.command])
                .disabled(!Zoom.canZoomIn)
            Button("Zoom Out") { Zoom.zoomOut() }
                .keyboardShortcut("-", modifiers: [.command])
                .disabled(!Zoom.canZoomOut)
            Button("Actual Size") { Zoom.reset() }
                .keyboardShortcut("0", modifiers: [.command])
                .disabled(prefs.zoom == 1)
            Divider()
            // Deliberately NO keyboardShortcut: a menu shortcut with no
            // modifier fires even while a text field has focus, so `\` is bound
            // in the key registry instead, behind its input guard.
            Button(prefs.theme == .dark ? "Light Appearance" : "Dark Appearance") {
                prefs.flipTheme()
            }
        }

        CommandGroup(replacing: .help) {
            Button("Keyboard Shortcuts") { store.shortcutsOpen = true }
                .keyboardShortcut("/", modifiers: [.command])
                // Not harmful, just untidy: the overlay would open UNDERNEATH
                // the re-triage modal and still be sitting there when the run
                // ends. Theme and Check for Updates stay live — neither
                // navigates, writes, nor leaves a surface behind.
                .disabled(blocked)
        }
    }
}
