// macOS notification delivery follows the server event feed. Tapping a
// notification opens its exact message, including auth and pending triage.
//
// The Event row is a denormalized snapshot, so a banner renders from the frame
// alone with no round trip. `EventBanner.copy(for:)` is that mapping, kept pure
// and in a file of its own because the notification extension posts the same
// banner from the same event. On a dev machine the grant resets every recompile: an ad-hoc
// signature's identity is a hash of the build.
//
// Every banner also carries the SENDER's mark as an attachment — see
// NotificationIcon. The icon at the left of a notification is the app's and
// always will be; the thumbnail beside the copy is the only place a banner gets
// to say who the mail is from, so that is what goes there. Drawing it is
// synchronous and stays that way: posting is the last thing that happens after
// a caller has already recorded the event as seen, so a post that waits on
// anything is a post that can be lost.
//
// EVERY BANNER NAMES ITS ACCOUNT. There is one event feed per account and the
// ids on those feeds are per-daemon SQLite ints, so two accounts hand this
// class the same event id and the same thread id for entirely unrelated mail.
// Both of the identifiers the system dedupes and groups on are therefore
// prefixed with the account uuid, and the uuid rides in `userInfo` so a tap can
// take the human to the mailbox the mail is actually in — switching accounts
// first if that is where it lives.

import UserNotifications
import os

// Fronting the app and finding its window are AppKit-only ideas, and MainWindow
// itself is a macOS file — the notification half below is what ships everywhere.
#if os(macOS)
    import AppKit
#endif

/// EVENTS THIS PROCESS HAS ALREADY TOLD THE READER ABOUT, by
/// `EventBanner.eventIdentity`: every notification presented while the app is
/// open, and every one tapped. The phone hears new mail twice (the daemon's
/// push and the app's own feed), and Notification Center alone cannot dedupe
/// them: a tapped push leaves it, and the feed's banner for the same mail
/// would then pop up as if new. Process memory is enough: a cold launch's
/// replay is the backlog rule's (`EventBanner.isBacklog`), and a tap that
/// launched the app lands here before the feed connects.
///
/// Remembered for TEN MINUTES, like the Notification Center check: past that
/// the two copies of one mail have long since both arrived, and an id reused
/// after a daemon's database is rebuilt must not be swallowed.
enum ToldEvents {
    private static let told = OSAllocatedUnfairLock(initialState: [String: Date]())
    private static let window: TimeInterval = 600

    static func note(_ identity: String) {
        let now = Date()
        told.withLock { seen in
            seen = seen.filter { now.timeIntervalSince($0.value) < window }
            seen[identity] = now
        }
    }

    static func contains(_ identity: String) -> Bool {
        let now = Date()
        return told.withLock { seen in
            guard let at = seen[identity] else { return false }
            return now.timeIntervalSince(at) < window
        }
    }
}

@MainActor
final class Notifier {
    static let shared = Notifier()

    /// UNUserNotificationCenter holds its delegate WEAKLY. This property is the
    /// only strong reference in the process — assigning a freshly-made delegate
    /// inline would deallocate it immediately and every tap would go nowhere.
    private let delegate = NotificationDelegate()
    private var asked = false

    private init() {}

    /// Install the delegate. Call from applicationDidFinishLaunching: a
    /// notification tapped while Passband was not running is delivered as soon as
    /// the delegate exists, and a center without one drops it.
    func install() {
        UNUserNotificationCenter.current().delegate = delegate
        Self.installSounds()
    }

    /// Copy the bundled chimes into ~/Library/Sounds. UNNotificationSound
    /// resolves names against that folder reliably; resolution against the app
    /// bundle is famously flaky on macOS, so the bundle is treated purely as the
    /// shipping vehicle. Idempotent: a copy that already matches by size is
    /// left alone, and any failure just means the system default plays.
    private static func installSounds() {
        let fm = FileManager.default
        guard let library = fm.urls(for: .libraryDirectory, in: .userDomainMask).first
        else { return }
        let sounds = library.appendingPathComponent("Sounds", isDirectory: true)
        try? fm.createDirectory(at: sounds, withIntermediateDirectories: true)
        for choice in NotificationSound.allCases {
            guard let resource = choice.resourceName, let installed = choice.installedFileName,
                let src = Bundle.main.url(
                    forResource: resource, withExtension: "caf", subdirectory: "Sounds")
            else { continue }
            let dst = sounds.appendingPathComponent(installed)
            let size = { (url: URL) in
                (try? fm.attributesOfItem(atPath: url.path)[.size] as? Int) ?? nil
            }
            if fm.fileExists(atPath: dst.path) {
                if size(src) == size(dst) { continue }
                try? fm.removeItem(at: dst)
            }
            try? fm.copyItem(at: src, to: dst)
        }
    }

    /// The UN sound for a preference choice. Falls back to the system default
    /// for `.system` (and, at delivery time, for an installed file gone missing).
    static func sound(for choice: NotificationSound) -> UNNotificationSound {
        guard let installed = choice.installedFileName else { return .default }
        return UNNotificationSound(named: UNNotificationSoundName(installed))
    }

    /// Ask for the alert/sound grant, once per launch. A denial is not worth
    /// surfacing — `post` simply becomes a no-op.
    func requestAuthorizationIfNeeded() async {
        guard !asked else { return }
        asked = true
        _ = try? await UNUserNotificationCenter.current()
            .requestAuthorization(options: [.alert, .sound])
    }

    // MARK: - posting

    /// Post one event's banner on behalf of one account. `EventBanner.copy(for:)`
    /// stays
    /// account-blind — the display copy is the same wherever the mail landed —
    /// and the account is folded into the two IDENTIFIERS below plus userInfo.
    func post(_ event: Event, accountId: UUID) {
        // A SEALED EVENT IS NOT MAIL WITH A THREAD TO OPEN. `EventStream` already
        // routes it to the auth path and never reaches here, and this guard is
        // the belt on top of that: the banner below would name the sender of a
        // login code, and its tap would ask for a thread the daemon refuses to
        // serve. Defensive on purpose — routing lives in ONE pure function
        // (`EventBanner.routing`) and a future caller that forgets to consult it
        // must fail closed rather than leak an auth arrival into the thread lane.
        guard case .threadBanner = EventBanner.routing(for: event) else { return }
        // A fresh login code floats over whatever app is in front instead of
        // sitting in a corner as a banner (see App/LoginCodePanel.swift). The
        // banner is what it falls back to when nobody answers the panel, so a
        // code is never in both places at once and never in neither.
        #if os(macOS)
            if let login = EventBanner.loginCode(for: event),
                EventBanner.floatsLoginCode(
                    enabled: Prefs.shared.floatLoginCodes,
                    respectFocus: Prefs.shared.loginCodesRespectFocus,
                    // Asked only when the setting wants it: the answer is the
                    // system's to give and nothing else here needs it.
                    focused: Prefs.shared.loginCodesRespectFocus ? FocusGate.focused : nil)
            {
                let accounts = AccountManager.shared.accounts
                LoginCodePanel.shared.show(
                    login, sender: event.sender,
                    account: accounts.count > 1
                        ? accounts.first { $0.id == accountId }?.displayName : nil,
                    // Silent: the panel already chimed for this code.
                    onMissed: { [weak self] in
                        self?.banner(event, accountId: accountId, silent: true)
                    })
                playChime()
                return
            }
        #endif
        banner(event, accountId: accountId)
    }

    #if os(macOS)
        /// Held for the duration of playback: the sound stops when deallocated.
        private var chime: NSSound?

        /// The chime the banner would have played. A panel that appears over
        /// another screen's work in silence is one nobody looks up for.
        private func playChime() {
            let choice = Prefs.shared.notificationSound
            if let resource = choice.resourceName,
                let url = Bundle.main.url(
                    forResource: resource, withExtension: "caf", subdirectory: "Sounds")
            {
                chime = NSSound(contentsOf: url, byReference: true)
                chime?.play()
            } else {
                NSSound.beep()
            }
        }
    #endif

    private func banner(_ event: Event, accountId: UUID, silent: Bool = false) {
        let account = accountId.uuidString
        let copy = EventBanner.copy(for: event)
        let content = UNMutableNotificationContent()
        content.title = copy.title
        content.body = copy.body
        // The coalescing group, NAMESPACED BY ACCOUNT. Thread ids come from the
        // daemon (and the fallback is built from an event id), so two accounts
        // can hand us the same one for unrelated mail — unprefixed, the system
        // would stack two mailboxes' banners into a single group as though they
        // were one conversation.
        content.threadIdentifier = "\(account).\(copy.threadIdentifier)"
        content.userInfo = [
            EventBanner.threadKey: event.thread_id,
            EventBanner.eventKey: event.id,
            EventBanner.messageKey: event.message_id,
            EventBanner.authKey: event.isAuth,
            EventBanner.accountKey: account,
        ]
        if event.isAuth { content.userInfo[EventBanner.senderKey] = event.sender }
        if copy.sound, !silent { content.sound = Self.sound(for: Prefs.shared.notificationSound) }

        // The event id as the REQUEST id makes a re-delivered frame (a replay
        // overlapping the live seam after a reconnect) replace its own banner
        // rather than stack a second copy. Also account-prefixed, and for a
        // sharper reason than the group is: event ids are per-daemon SQLite
        // ints, so account B's event 41 would REPLACE account A's event 41 —
        // one banner silently eating the other.
        // A read receipt draws NO tile. `copy(for:)` goes out of its way not to
        // read as mail from yourself, and `sender` on an `.opened` row is the
        // account's own address — so an avatar of the reader is exactly the
        // impression the copy is avoiding, and if their own local-part happens
        // to be a robot shape (support@, billing@, hello@) it would send their
        // own domain out to be looked up to decorate a receipt about their own
        // sent mail.
        send(
            content, identifier: "passband.event.\(account).\(event.id)",
            sender: event.kind == .opened ? "" : event.sender)
    }

    /// Attach the sender's mark and hand the banner to the system.
    ///
    /// SYNCHRONOUS, and the comment is here because the other shape is so
    /// tempting: waiting on a first-sight domain logo would make every banner
    /// prettier and some of them late or missing. Both callers record the event
    /// as seen BEFORE they post — `EventStream` writes its cursor, the auth
    /// watcher saves its id set — so a post deferred behind anything at all is
    /// a post a quit can swallow with nothing left to retry it. `postAuth`'s
    /// callers also depend on the ORDER holding: they sort oldest-first so the
    /// newest code lands on top of the stack, which only survives if posting is
    /// straight-line. The tile draws with the logo already in hand, and `warm`
    /// goes and gets the one that was not.
    private func send(_ content: UNMutableNotificationContent, identifier: String, sender: String) {
        content.attachments = NotificationIcon.attachments(for: sender, id: identifier)
        let request = UNNotificationRequest(identifier: identifier, content: content, trigger: nil)
        // The system takes ownership of a tile only once it accepts the request
        // carrying it. A rejected request leaves the bytes with us, and a PNG
        // per refused banner is exactly the accumulation the tile root exists
        // to prevent.
        let tiles = content.attachments.map(\.url)
        UNUserNotificationCenter.current().add(request) { error in
            guard error != nil else { return }
            Task { @MainActor in NotificationIcon.discard(tiles) }
        }
        NotificationIcon.warm(sender)
    }

    /// Post a banner shaped exactly like the real thing, on demand. Settings
    /// offers it for the same reason the sound picker plays its chime: whether
    /// notifications actually arrive — past the system's own permission, focus
    /// modes and Do Not Disturb — is not a question anyone should have to
    /// answer by waiting for mail.
    ///
    /// It is addressed FROM PASSBAND, so the tile it draws is the initials
    /// fallback: `hello@` is a robot local-part, but passband.app is not in the
    /// icon service, and a domain that answers nothing is remembered as such
    /// for a week.
    ///
    /// Returns whether the system will SHOW it, which is not the same as
    /// whether it was posted — the grant is the one refusal worth reporting,
    /// because a test that does nothing in exactly that case is not a test.
    /// Focus modes and Do Not Disturb are not visible from here and are what
    /// the Settings hint points at when this returns true and nothing appears.
    @discardableResult
    func postTest() async -> Bool {
        await requestAuthorizationIfNeeded()
        let status = await UNUserNotificationCenter.current()
            .notificationSettings().authorizationStatus
        guard status == .authorized || status == .provisional else { return false }

        let content = UNMutableNotificationContent()
        content.title = "Passband"
        content.subtitle = "test banner"
        content.body = "Notifications are working. This is what mail worth reading looks like."
        content.threadIdentifier = "passband.test"
        content.sound = Self.sound(for: Prefs.shared.notificationSound)
        // The one banner that names no account, because it belongs to none.
        // Both identifiers are still unique against every real one — an event's
        // are "passband.event.<uuid>.<id>" and a group is "<uuid>.<thread>", so
        // nothing this posts can group with, or replace, a piece of mail. It
        // does replace ITSELF, which is what a second press should do.
        //
        // The route marker is load-bearing twice over: `presentation` draws
        // this one even with the app frontmost — Settings is a page in the main
        // window, so the ordinary rule would file the test banner silently into
        // Notification Center and the button would look broken — and the
        // delegate reads it to keep a self-test out of the open-rate metric.
        content.userInfo = [EventBanner.routeKey: EventBanner.testRoute]
        send(content, identifier: "passband.test", sender: "Passband <hello@passband.app>")
        return true
    }

    /// A new version of Passband is ready to install. Posted by the updater
    /// when a SCHEDULED check finds one; the tap brings the window forward,
    /// and the update card inside it is the install button.
    ///
    /// Presented by the ordinary foreground rule on purpose: with the app
    /// frontmost and its window up, the card is already on screen and a banner
    /// saying the same thing would be the second copy.
    ///
    /// No sender tile and no chime. It is the app talking about itself, and a
    /// release is not urgent enough to make a sound.
    func postUpdate(version: String) async {
        await requestAuthorizationIfNeeded()
        let content = UNMutableNotificationContent()
        content.title = "Passband \(version) is ready"
        content.body = "Click to open Passband and install it. It takes a few seconds."
        content.threadIdentifier = "passband.update"
        content.userInfo = [EventBanner.routeKey: EventBanner.updateRoute]
        // One identifier for every version, so a newer release replaces an
        // unread banner for an older one instead of stacking beside it.
        send(content, identifier: "passband.update", sender: "")
    }

    // MARK: - delegate behaviour

    /// What to do with a notification that arrives while the app is running.
    /// Frontmost WITH a visible window => no banner, only the list: the human is
    /// already looking at the sitrep the row is on. The window check matters —
    /// "active with zero visible windows" is a common state under residency, and
    /// there the banner is the app's only voice.
    ///
    /// The test banner is the exception, and it has to be: it is posted from a
    /// button on a page INSIDE the main window, so the rule above would suppress
    /// it every single time it is used. A banner nobody asked for is noise while
    /// you are looking at the list it came from; a banner you just pressed a
    /// button for is the whole point.
    nonisolated static func presentation(
        appActive: Bool, windowVisible: Bool, isTest: Bool = false, isAuth: Bool = false
    ) -> UNNotificationPresentationOptions {
        EventBanner.shouldPresentForeground(appActive: appActive, windowVisible: windowVisible,
            isTest: isTest, isAuth: isAuth) ? [.banner, .sound, .list] : [.list]
    }

    /// Where a tap lands once the right mailbox is on screen.
    private enum TapTarget {
        case thread(String?)
        case message(Int)
        case event(Int)
        case auth
        /// Auth mail with a message id: the audited reveal, never the reader.
        case reveal(messageId: Int, threadId: String, sender: String)
    }

    /// A tap: front the app, restore the window if it was closed, open the
    /// thread — in the account the banner was posted from, switching to it
    /// first when that is not the account currently on screen.
    func handleEventTap(eventId: Int, accountId: UUID) {
        Analytics.capture("notification_opened", ["has_thread": true])
        deliver(.event(eventId), accountId: accountId)
    }

    /// A tap on an auth banner that names its message: the one-time reveal,
    /// built from the payload so it works on a cold launch, before the first
    /// sealed list has landed. No body is fetched until the panel asks.
    func handleRevealTap(messageId: Int, threadId: String, sender: String, accountId: UUID?) {
        Analytics.capture("notification_opened", ["has_thread": true])
        deliver(.reveal(messageId: messageId, threadId: threadId, sender: sender), accountId: accountId)
    }

    func handleMessageTap(messageId: Int, accountId: UUID?) {
        Analytics.capture("notification_opened", ["has_thread": true])
        deliver(.message(messageId), accountId: accountId)
    }

    func handleTap(threadId: String?, accountId: UUID?) {
        Analytics.capture("notification_opened", ["has_thread": !(threadId ?? "").isEmpty])
        deliver(.thread(threadId), accountId: accountId)
    }

    /// A tap on an auth banner that names no message: the Auth view of the
    /// mailbox it arrived in, where the human asks for the mail themselves. A
    /// banner that does name its message opens the reveal instead
    /// (`handleRevealTap`), audited with source "notification" so the daemon's
    /// pairing never counts a tap as "the owner used this code".
    ///
    /// The same analytics event as a thread tap, because it is the same act (a
    /// banner opened) and the bool already says which shape it had.
    func handleAuthTap(accountId: UUID?) {
        Analytics.capture("notification_opened", ["has_thread": false])
        deliver(.auth, accountId: accountId)
    }

    private var tapQueue = NotificationTapQueue<TapTarget>()
    private var deliveringTap = false

    private func deliver(_ target: TapTarget, accountId: UUID?) {
        tapQueue.enqueue(target, accountId: accountId)
        front()
        drainPendingTap()
    }

    func connectionBecameReady() {
        tapQueue.connectionBecameReady()
        drainPendingTap()
    }

    /// Called only after the shared client has working credentials and the
    /// daemon's v2 contract has been verified. Failed connections retain taps.
    func drainPendingTap() {
        let store = AppStore.shared
        guard !deliveringTap, !store.switching, !store.accountActionsBlocked,
            let tap = tapQueue.take(connected: store.connStatus == .connected)
        else { return }
        guard let account = tap.accountId ?? AccountManager.shared.activeId,
            AccountManager.shared.accounts.contains(where: { $0.id == account })
        else { return }
        if account == AccountManager.shared.activeId {
            open(tap.target)
            return
        }
        deliveringTap = true
        Task {
            defer {
                deliveringTap = false
                drainPendingTap()
            }
            await AccountManager.shared.switchTo(account)
            guard AccountManager.shared.activeId == account, store.connStatus == .connected else {
                // Retry after reconnect; a newer tap always wins.
                tapQueue.park(tap)
                return
            }
            open(tap.target)
        }
    }

    /// Bring the app forward. AppKit-only, and a no-op elsewhere: on the phone
    /// the tap has already foregrounded the app by the time this runs.
    ///
    /// Not private: a tapped test banner does this and nothing else.
    func front() {
        #if os(macOS)
            NSApp.activate(ignoringOtherApps: true)
            MainWindow.show()
        #endif
    }

    private func open(_ target: TapTarget) {
        switch target {
        case .thread(let threadId):
            guard let threadId, !threadId.isEmpty else { return }
            AppStore.shared.openThread(threadId, from: .notification)
        case .message(let messageId):
            AppStore.shared.openMessage(messageId, from: .notification)
        case .event(let eventId):
            let store = AppStore.shared
            let epoch = store.epoch
            Task {
                do {
                    let event = try await APIClient.shared.getEvent(eventId)
                    guard store.isCurrent(epoch) else { return }
                    // An event on auth mail opens the reveal, never the reader.
                    store.openMessage(
                        event.message_id, from: .notification,
                        auth: event.isAuth ? AuthPolicy.authMeta(for: event) : nil)
                } catch {
                    guard store.isCurrent(epoch) else { return }
                    store.pushToast("Could not open this notification. Please try again.", .error)
                }
            }
        case .auth:
            // The routed Auth page on the Mac; on the phone, the login-codes
            // page behind the Quick Look tab's key.
            AppStore.shared.requestAuthPage()
        case .reveal(let messageId, let threadId, let sender):
            let store = AppStore.shared
            store.requestReveal(
                store.sealedMeta(message: messageId)
                    ?? AuthPolicy.revealMeta(messageId: messageId, threadId: threadId, sender: sender),
                source: RevealSource.notification)
        }
    }
}

/// The center's delegate, retained by `Notifier.shared`.
///
/// COMPLETION-HANDLER METHODS, NOT THE ASYNC OVERLOADS. The async forms of
/// these two requirements are nonisolated, so Swift runs them on the global
/// executor and calls the system's completion handler from a background
/// thread. AppKit tolerates that; UIKit does not — it raises "Call must be
/// made on main thread" and a tapped banner killed the phone app on launch.
/// The completion forms are delivered on the main queue, so each method reads
/// the payload, does its main-actor work, and answers, all on that queue.
///
/// Not main-actor isolated as a class: the protocol has no isolation of its
/// own, and an isolated conformance would trap rather than recover if a
/// callback ever arrived elsewhere. `onMain` hops instead.
final class NotificationDelegate: NSObject, UNUserNotificationCenterDelegate {
    /// Run `work` on the main actor: inline when already on the main thread
    /// (the documented case), otherwise queued there rather than trapping.
    private func onMain(_ work: @escaping @MainActor () -> Void) {
        if Thread.isMainThread {
            MainActor.assumeIsolated(work)
        } else {
            DispatchQueue.main.async { MainActor.assumeIsolated(work) }
        }
    }

    func userNotificationCenter(
        _ center: UNUserNotificationCenter, willPresent notification: UNNotification,
        withCompletionHandler completionHandler:
            @escaping (UNNotificationPresentationOptions) -> Void
    ) {
        let route = notification.request.content.userInfo[EventBanner.routeKey] as? String
        let isAuth = notification.request.content.userInfo[EventBanner.authKey] as? Bool ?? false
        nonisolated(unsafe) let done = completionHandler
        #if os(iOS)
            // ONE BANNER PER EVENT. The phone hears new mail twice while it is
            // open: the daemon pushes every event, and the app's own feed
            // posts a banner for it too, under a different identifier, so the
            // system would show both. Whichever arrives SECOND finds the first
            // already in Notification Center and is shown as nothing.
            //
            // Two records answer "already told": this process's own
            // (`ToldEvents`, which a TAP also writes, since a tapped push
            // leaves Notification Center), and Notification Center for what
            // arrived before this process heard it. Only the last ten minutes
            // of the second count: event ids restart if a daemon's database is
            // rebuilt, and yesterday's "event 41" must not swallow today's.
            //
            // ANSWERED ON MAIN. `getDeliveredNotifications` calls back on a
            // background queue, and answering this handler off main has killed
            // the phone app before ("Call must be made on main thread").
            if let identity = EventBanner.eventIdentity(notification.request.content.userInfo) {
                let own = notification.request.identifier
                if ToldEvents.contains(identity) {
                    DispatchQueue.main.async { done([]) }
                    return
                }
                center.getDeliveredNotifications { delivered in
                    let recent = Date().addingTimeInterval(-600)
                    let told = delivered.contains {
                        $0.request.identifier != own && $0.date > recent
                            && EventBanner.eventIdentity($0.request.content.userInfo) == identity
                    }
                    ToldEvents.note(identity)
                    // The phone's own rule, as below: no foreground check yet.
                    let options = told
                        ? []
                        : Notifier.presentation(
                            appActive: false, windowVisible: false,
                            isTest: route == EventBanner.testRoute, isAuth: isAuth)
                    DispatchQueue.main.async { done(options) }
                }
                return
            }
        #endif
        onMain {
            #if os(macOS)
                let (active, visible) = (NSApp.isActive, MainWindow.find()?.isVisible == true)
            #else
                // Off macOS the banner always wins until there is a real
                // foreground check: a suppressed notification is a lost one.
                let (active, visible) = (false, false)
            #endif
            done(
                Notifier.presentation(
                    appActive: active, windowVisible: visible,
                    isTest: route == EventBanner.testRoute, isAuth: isAuth))
        }
    }

    func userNotificationCenter(
        _ center: UNUserNotificationCenter, didReceive response: UNNotificationResponse,
        withCompletionHandler completionHandler: @escaping () -> Void
    ) {
        let userInfo = response.notification.request.content.userInfo
        // A tapped notification has been told, and has left Notification
        // Center: remember it so the feed's banner for it stays quiet.
        if let identity = EventBanner.eventIdentity(userInfo) { ToldEvents.note(identity) }
        // Decided from the payload alone, BEFORE anything is fetched: an auth
        // tap opens the reveal (or the Auth page) and never `getMessage`.
        let tap = EventBanner.tapRoute(userInfo)
        // Stored as a string and parsed here rather than crossing as one: a
        // payload that survived a relaunch (or came from an older build) can
        // hold anything, and an unparseable id must read as "no account", not
        // as some other account.
        let accountId = (userInfo[EventBanner.accountKey] as? String).flatMap(UUID.init(uuidString:))
        nonisolated(unsafe) let done = completionHandler
        onMain {
            // Answered once the tap is routed, whichever branch returns.
            defer { done() }
            switch tap {
            case .update:
                Notifier.shared.front()
                // A banner from before a quit: this launch has no card yet, and
                // Sparkle's next scheduled check could be hours off. The tap is
                // a request, so it gets the check a request gets.
                #if os(macOS)
                    if Updater.shared.phase == .idle { Updater.shared.check() }
                #endif
            // A self-test opens nothing and is COUNTED as nothing: routing it
            // through handleTap would file a `notification_opened` and quietly
            // inflate the one metric that says whether banners are worth
            // posting at all.
            case .test:
                Notifier.shared.front()
            case .reveal(let messageId, let threadId, let sender):
                Notifier.shared.handleRevealTap(
                    messageId: messageId, threadId: threadId, sender: sender, accountId: accountId)
            case .authPage:
                Notifier.shared.handleAuthTap(accountId: accountId)
            case .message(let messageId):
                Notifier.shared.handleMessageTap(messageId: messageId, accountId: accountId)
            case .event(let unresolved):
                Notifier.shared.handleEventTap(eventId: unresolved.eventId, accountId: unresolved.accountId)
            case .thread(let threadId):
                Notifier.shared.handleTap(threadId: threadId, accountId: accountId)
            }
        }
    }
}
