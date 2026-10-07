// Sparkle auto-update wearing Passband's own face. One updater for the process
// (Sparkle asserts if a second one starts for the same bundle), and one card
// inside the app instead of Sparkle's separate window.
//
// Scheduled checks run hourly and download silently: the new version is
// fetched and unpacked in the background, installs itself on the next quit,
// and the card only appears once it is ready, offering a restart into it now.
// A check the human asks for from the menu still goes through the user driver
// below, where a new version arrives as an in-app alert whose single button
// installs it and relaunches.
//
// This is a full SPUUserDriver rather than the standard driver plus its
// gentle-reminder delegate. That delegate can only DEFER Sparkle's own alert,
// and that alert is exactly the second click being removed. What the standard
// driver used to own is reproduced here on purpose, not dropped: the two
// answers a check the human asked for has to get (up to date / it failed).
// A BACKGROUND failure stays silent, which is the one thing a driver with no
// window of its own can do that Sparkle's cannot.
//
// The vendored framework is prebuilt (vendor/fetch-sparkle.sh); the feed URL
// and the EdDSA public key live in Info.plist.

import AppKit
import Combine
import Sparkle
import UserNotifications

/// Where the update flow has got to, as the card needs to see it. `available`
/// is the ONLY phase that waits on a human; everything past it is progress the
/// card narrates while Sparkle works, and every one of them ends at a relaunch.
enum UpdatePhase: Equatable {
    case idle
    /// A check the human asked for, in flight. Never entered for a scheduled
    /// check: the whole point of those is that they cost no attention until
    /// there is something to say.
    case checking
    /// Sparkle has an update and is holding for an answer.
    case available(version: String)
    /// Downloaded in the background and waiting for the next quit. Restart
    /// installs it now; Later only hides the card, since the quit still will.
    case ready(version: String)
    /// nil fraction: the feed declared no length, so there is a download but no
    /// honest way to draw how much of it is done.
    case downloading(Double?)
    case extracting(Double)
    /// Past the point of no return: the app is being replaced and relaunched.
    case installing

    /// Whether the card is on screen at all.
    var visible: Bool { self != .idle }

    /// Whether the flow is running itself, i.e. the card narrates and its
    /// buttons are gone.
    var busy: Bool {
        switch self {
        case .idle, .checking, .available, .ready: false
        case .downloading, .extracting, .installing: true
        }
    }
}

@MainActor
final class Updater: NSObject, ObservableObject, SPUUserDriver, SPUUpdaterDelegate {
    static let shared = Updater()

    /// Sparkle's engine. Implicitly unwrapped because it is constructed with
    /// `self` as its user driver, which cannot happen until `super.init()` has
    /// run and every stored property already has a value.
    private var engine: SPUUpdater!

    /// Mirrors Sparkle's own gate (KVO), so the menu item disables itself
    /// mid-check instead of stacking a second session on a click.
    @Published private(set) var canCheck = false

    /// What the card draws.
    @Published private(set) var phase: UpdatePhase = .idle {
        // The banner announces the card, so it goes when the card does (Later,
        // an install, a failed download). Left behind, it is a click that
        // opens a window with nothing in it.
        didSet {
            if phase == .idle, oldValue != .idle {
                UNUserNotificationCenter.current()
                    .removeDeliveredNotifications(withIdentifiers: ["passband.update"])
            }
        }
    }

    /// The version waiting to be installed, for the rail's update tile. Outlives
    /// the card on purpose: Later hides the card, never this. Nil once a check
    /// says the app is current; a relaunch into the update clears it by
    /// starting a fresh process.
    @Published private(set) var pending: String?

    /// Sparkle's question, parked until the human answers it. `install` is the
    /// card's one button; `dismiss` is Later, which brings the update back on
    /// the next scheduled check rather than burying the version forever, which
    /// is what `skip` would do. Nil whenever nothing is waiting on an answer.
    private var choice: ((SPUUserUpdateChoice) -> Void)?

    /// Sparkle's install-now handle for an update it downloaded silently. Nil
    /// unless `phase` is `.ready`. Sparkle installs on quit regardless; this
    /// only lets the card do it sooner.
    private var installNow: (() -> Void)?

    /// Later's alarm: the card comes back this long after it was waved off.
    private static let remindAfter: Duration = .seconds(60 * 60)
    private var reminder: Task<Void, Never>?

    /// Set the moment the human presses Update. This is what makes the second
    /// prompt unnecessary: `showReady(toInstallAndRelaunch:)` answers itself
    /// only because consent for this exact update was already given, and a
    /// relaunch nobody asked for is the one failure mode worth guarding.
    private var consented = false

    /// True only while a check the human started is running. Errors and "no new
    /// version" are reported ONLY under it: a scheduled check that cannot reach
    /// the feed is not news, it is a laptop on a plane.
    private var userInitiated = false

    private var downloadTotal: UInt64 = 0
    private var downloadSoFar: UInt64 = 0

    private override init() {
        super.init()
        // A dev build is not an install: it carries the release's bundle id
        // and feed, and its build number (the commit count of a branch) sits
        // below the shipped one, so Sparkle read it as out of date, replaced
        // the bundle in the worktree with the notarized release, and left a
        // staged update in the cache every other copy shares, which is how
        // the real app came to offer the version it was already running. Only
        // a shipped build (build-release.sh defines PASSBAND_RELEASE) updates
        // itself. A dev build does only when PASSBAND_UPDATER is set on
        // purpose, which is how updater work gets tested. Not started leaves
        // `canCheck` false, which greys the menu item.
        #if !PASSBAND_RELEASE
            guard ProcessInfo.processInfo.environment["PASSBAND_UPDATER"] != nil else { return }
        #endif
        let engine = SPUUpdater(
            hostBundle: .main, applicationBundle: .main, userDriver: self, delegate: self)
        self.engine = engine
        // Always check, always download silently. Both are forced on EVERY
        // launch, not left to Info.plist: the preferences persist in user
        // defaults, and older builds asked (and forced downloads off), so an
        // existing install can carry a "no" to either. In automatic-download
        // mode SPUAutomaticUpdateDriver never calls the user driver; the one
        // place this file hears about the update is
        // `updater(_:willInstallUpdateOnQuit:immediateInstallationBlock:)`.
        engine.automaticallyChecksForUpdates = true
        engine.automaticallyDownloadsUpdates = true
        do {
            try engine.start()
        } catch {
            // A misconfigured feed is a build mistake, not a runtime condition
            // to nag about: the app works fine, it just cannot update itself.
            // Log it and leave `canCheck` false, which greys the menu item.
            NSLog("passband: updater did not start: \(error.localizedDescription)")
            return
        }
        engine.publisher(for: \.canCheckForUpdates)
            .receive(on: DispatchQueue.main)
            .assign(to: &$canCheck)
    }

    // MARK: - what the app calls

    /// User-initiated check, from the app menu.
    func check() {
        userInitiated = true
        engine?.checkForUpdates()
    }

    /// THE button. Consent for this update, once, covering both of the answers
    /// Sparkle asks for: install it, and relaunch into it.
    func installAndRelaunch() {
        guard let reply = choice else { return }
        consented = true
        choice = nil
        phase = .downloading(nil)
        reply(.install)
    }

    /// The rail tile: put the card back in front of the human. A downloaded
    /// update just re-shows its card; one that was only offered had its
    /// Sparkle session closed by Later, so it takes a check to ask again.
    func showAgain() {
        guard phase == .idle, let pending else { return }
        reminder?.cancel()
        if installNow != nil {
            phase = .ready(version: pending)
        } else {
            check()
        }
    }

    /// Restart into an update that downloaded in the background.
    func restartNow() {
        guard let installNow else { return }
        reminder?.cancel()
        phase = .installing
        installNow()
    }

    /// Later: the card goes, and comes back an hour from now. `dismiss`, not
    /// `skip`, because "not now" is not "never".
    func later() {
        if case .ready(let version) = phase {
            // Downloaded and waiting for the quit, so there is nothing to
            // re-check: the same card simply comes back.
            phase = .idle
            remind { $0.phase = .ready(version: version) }
            return
        }
        if let reply = choice {
            choice = nil
            reply(.dismiss)
        }
        phase = .idle
        // Sparkle's session is over, so coming back means asking again. A
        // background check lands on the silent path (a `.ready` card) or, for
        // an update that cannot install unattended, `showUpdateFound`.
        remind { $0.engine?.checkForUpdatesInBackground() }
    }

    /// Runs `bringBack` after `remindAfter`, unless the card has come back some
    /// other way in the meantime (a menu check, a newer download).
    private func remind(_ bringBack: @escaping @MainActor (Updater) -> Void) {
        reminder?.cancel()
        reminder = Task { [weak self] in
            try? await Task.sleep(for: Self.remindAfter)
            guard let self, !Task.isCancelled, self.phase == .idle else { return }
            bringBack(self)
        }
    }

    // MARK: - SPUUserDriver: permission

    /// Never asked in practice: `SUEnableAutomaticChecks` in Info.plist tells
    /// Sparkle the answer already. Required by the protocol, so it gives the
    /// same answer without a modal. The system profile is never sent: it is a
    /// hardware and locale fingerprint, and none of it makes the feed better
    /// at telling this app there is a newer zip.
    func show(
        _ request: SPUUpdatePermissionRequest,
        reply: @escaping (SUUpdatePermissionResponse) -> Void
    ) {
        reply(SUUpdatePermissionResponse(automaticUpdateChecks: true, sendSystemProfile: false))
    }

    // MARK: - SPUUpdaterDelegate: the silent path

    /// A scheduled check downloaded and unpacked an update with nobody asked.
    /// YES takes over the install: Sparkle holds the update cycle (no point
    /// checking hourly for a version already waiting) and installs on quit
    /// regardless, and `immediateInstallHandler` lets the card do it now.
    func updater(
        _ updater: SPUUpdater, willInstallUpdateOnQuit item: SUAppcastItem,
        immediateInstallationBlock immediateInstallHandler: @escaping () -> Void
    ) -> Bool {
        let version = item.displayVersionString
        reminder?.cancel()
        installNow = immediateInstallHandler
        pending = version
        phase = .ready(version: version)
        // Same once-per-version banner as a found update: the card only speaks
        // to someone looking at the window.
        if Self.bannered != version {
            Self.bannered = version
            Task { await Notifier.shared.postUpdate(version: version) }
        }
        return true
    }

    // MARK: - SPUUserDriver: the check

    func showUserInitiatedUpdateCheck(cancellation: @escaping () -> Void) {
        userInitiated = true
        phase = .checking
    }

    func showUpdateFound(
        with appcastItem: SUAppcastItem,
        state: SPUUserUpdateState,
        reply: @escaping (SPUUserUpdateChoice) -> Void
    ) {
        // An information-only update has no zip to install — the appcast is
        // pointing at a page instead. There is no honest "Update" button for
        // that, so hand it to the browser and end the session.
        if appcastItem.isInformationOnlyUpdate {
            if let url = appcastItem.infoURL { NSWorkspace.shared.open(url) }
            phase = .idle
            reply(.dismiss)
            return
        }
        reminder?.cancel()
        choice = reply
        consented = false
        let version = appcastItem.displayVersionString
        pending = version
        phase = .available(version: version)
        // The card only speaks to someone looking at the window. A scheduled
        // check usually lands while Passband sits behind other apps or with its
        // window closed, so the news goes out as a banner too, once per
        // version: Later brings the card back on every later check, and a
        // banner each time would be nagging. A check the human asked for gets
        // none, because they are already watching the answer.
        if !state.userInitiated, Self.bannered != version {
            Self.bannered = version
            Task { await Notifier.shared.postUpdate(version: version) }
        }
    }

    /// The last version a banner went out for. Persisted so a relaunch does
    /// not announce the same release twice.
    private static var bannered: String? {
        get { UserDefaults.standard.string(forKey: "passband.update.bannered") }
        set { UserDefaults.standard.set(newValue, forKey: "passband.update.bannered") }
    }

    func showUpdateNotFoundWithError(_ error: Error, acknowledgement: @escaping () -> Void) {
        phase = .idle
        // Current after all: whatever the tile was pointing at is gone.
        if installNow == nil { pending = nil }
        // Silence unless someone is waiting to hear it.
        if userInitiated {
            let alert = NSAlert()
            alert.messageText = "Passband is up to date."
            alert.informativeText = "You're on the newest version."
            alert.addButton(withTitle: "OK")
            alert.runModal()
        }
        userInitiated = false
        acknowledgement()
    }

    func showUpdaterError(_ error: Error, acknowledgement: @escaping () -> Void) {
        phase = .idle
        choice = nil
        // `consented` as well as `userInitiated`: pressing Update is a request
        // too, and a download that dies afterwards would otherwise take the
        // card off screen and say nothing, which reads as the update having
        // been applied. Read before the reset below.
        let owed = userInitiated || consented
        consented = false
        if owed {
            let alert = NSAlert()
            alert.messageText = "Couldn't check for updates."
            alert.informativeText = error.localizedDescription
            alert.addButton(withTitle: "OK")
            alert.runModal()
        } else {
            NSLog("passband: update check failed: \(error.localizedDescription)")
        }
        userInitiated = false
        acknowledgement()
    }

    // MARK: - SPUUserDriver: release notes

    // Nothing to do with either: the card is one line and a button, and the
    // appcast this app publishes carries no notes to put in it. Sparkle calls
    // these whether or not anyone is listening.
    func showUpdateReleaseNotes(with downloadData: SPUDownloadData) {}
    func showUpdateReleaseNotesFailedToDownloadWithError(_ error: Error) {}

    // MARK: - SPUUserDriver: the work

    func showDownloadInitiated(cancellation: @escaping () -> Void) {
        downloadTotal = 0
        downloadSoFar = 0
        phase = .downloading(nil)
    }

    func showDownloadDidReceiveExpectedContentLength(_ expectedContentLength: UInt64) {
        downloadTotal = expectedContentLength
    }

    func showDownloadDidReceiveData(ofLength length: UInt64) {
        downloadSoFar += length
        // A zip that turns out longer than advertised must not read as 140%.
        phase = .downloading(
            downloadTotal > 0
                ? min(1, Double(downloadSoFar) / Double(downloadTotal)) : nil)
    }

    func showDownloadDidStartExtractingUpdate() {
        phase = .extracting(0)
    }

    func showExtractionReceivedProgress(_ progress: Double) {
        phase = .extracting(min(1, max(0, progress)))
    }

    /// The second question, answered from the consent already given for THIS
    /// update. Without `consented` a relaunch could arrive unasked — from an
    /// update downloaded in a previous session, say — so the guard stands and
    /// the card (which Sparkle raised through `showUpdateFound` first) keeps
    /// waiting for a human.
    func showReady(toInstallAndRelaunch reply: @escaping (SPUUserUpdateChoice) -> Void) {
        guard consented else {
            choice = reply
            return
        }
        phase = .installing
        reply(.install)
    }

    func showInstallingUpdate(
        withApplicationTerminated applicationTerminated: Bool,
        retryTerminatingApplication: @escaping () -> Void
    ) {
        phase = .installing
    }

    func showUpdateInstalledAndRelaunched(
        _ relaunched: Bool, acknowledgement: @escaping () -> Void
    ) {
        phase = .idle
        acknowledgement()
    }

    // MARK: - SPUUserDriver: session end

    func dismissUpdateInstallation() {
        // Sparkle is done with this session, one way or another. Anything the
        // card was narrating is over; a parked reply here is a question nobody
        // will answer any more.
        choice = nil
        consented = false
        userInitiated = false
        // A silently downloaded update outlives the session that fetched it:
        // it is still waiting for the quit, so its card stays.
        if case .ready = phase { return }
        phase = .idle
    }

    /// Sparkle asking for the update it already raised to be put back in front
    /// of the human — a second ⌘-check while the card is up. The card lives in
    /// the window, so bringing the window forward IS showing it.
    func showUpdateInFocus() {
        MainWindow.show()
    }
}
