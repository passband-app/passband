// THE LAUNCH WARM: pull the sitrep's images onto disk before the reader opens
// anything, and pin them to the mail they belong to so they stay until it is done.
//
// ORDER MATTERS: for-your-eyes mail warms first, before a single marketing image.
// And WHAT IS ON SCREEN goes first of all: the rows the board shows before its
// expanders are warmed at once. The rest waits for a quiet stretch, pauses for
// any reader opened while it runs, and stops on Low Power Mode, on a network
// that costs the person, or with remote images turned off. Neither half asks
// for a thread ThreadPrefetch is leaving alone. Up to two hundred
// threads' bodies and images at launch were contending with the first click
// for the link, the CPU and the daemon.
//
// The remote-images pref is an ABSOLUTE gate — a prefetch is exactly the request
// that pref exists to prevent, and doing it invisibly into a cache would be
// worse, not better; reconciliation still runs, being network-free. Fires once
// per store epoch (a launch, an account switch, a practice entry), once BOTH the
// bands and the zones have landed under it (either alone warms half the list
// and never returns for the rest).

import Foundation
import Network

/// How many rows each board list shows before its fold. One table, because the
/// launch warm reaches for exactly what is on screen and must agree with the
/// views about what that is: SitrepView, MobileSitrepView and ReadingZone all
/// read it.
enum BoardFold {
    /// For-your-eyes rows before the "{n} more" expander. Four on the phone,
    /// well short of the Mac's ten: a phone's fold is shorter, and four ranked
    /// rows plus the expander still fit under it with the hero.
    #if os(iOS)
        static let forYourEyes = 4
    #else
        static let forYourEyes = 10
    #endif
    /// Reading cards before that zone's expander. The phone draws two-up, so
    /// the Mac's 24 would be twelve rows of cards before the fold; 8 is four
    /// rows, in line with its For-your-eyes fold.
    #if os(iOS)
        static let reading = 8
    #else
        static let reading = 24
    #endif
}

@MainActor
final class ImageWarmer {
    static let shared = ImageWarmer()

    /// Concurrent thread warms during the launch pass — the same width the
    /// per-image fetches inside each thread use: wide enough to hide latency,
    /// narrow enough not to saturate the link at launch.
    private static let warmWidth = 4

    /// When the rest of the warm goes: once nobody has been busy (see `busy`)
    /// for `settle`, counted in `sample`s, so time the app spends suspended
    /// counts for one sample at most; while somebody is, it looks again every
    /// `recheck`. Settable for the suite.
    struct Deferral {
        var settle: Duration = .seconds(30)
        var sample: Duration = .seconds(1)
        var recheck: Duration = .seconds(15)
    }
    var deferral = Deferral()
    /// Whether the rest may not go: Low Power Mode, a network path that
    /// costs the person (cellular, a personal hotspot, Low Data Mode), or no
    /// network at all (a hero that fails is remembered as having no art).
    /// Settable for the suite.
    var constrained: @MainActor (PathWatch) -> Bool = { path in
        ProcessInfo.processInfo.isLowPowerModeEnabled || path.isCostly || path.isOffline
    }
    /// The deferred half of the last warm. Cancelled by an account switch and
    /// by the next warm's; held for the suite to wait on.
    private(set) var deferred: Task<Void, Never>?

    /// The store epoch each half last landed under, and the one the last warm
    /// was for. A warm stops at an epoch bump, so both halves landing under a
    /// new epoch start another, the bump a switch's or not (a practice entry
    /// cancelled while its drafts settled bumps the epoch and keeps the board).
    private var sitrepEpoch: Int?
    private var zonesEpoch: Int?
    private var startedEpoch: Int?

    private init() {}

    func noteSitrepLanded() {
        sitrepEpoch = AppStore.shared.epoch
        startIfReady()
    }

    func noteZonesLanded() {
        zonesEpoch = AppStore.shared.epoch
        startIfReady()
    }

    private func startIfReady() {
        // The account this warm is for is the one whose board just landed,
        // read here and not when the task gets to run.
        let store = AppStore.shared
        let epoch = store.epoch
        guard sitrepEpoch == epoch, zonesEpoch == epoch, startedEpoch != epoch else { return }
        startedEpoch = epoch
        Task { await warmSitrepOnce(store: store, epoch: epoch) }
    }

    /// An account switch. The warm is once per WORLD, not once per process:
    /// the new account's bands and zones land next and have their own images
    /// to pull down — and their own pins, which is the half that matters, since
    /// reconciling against the OLD sitrep would unpin every image the new
    /// account's mail still needs. The old world's deferred half goes now.
    func resetForSwitch() {
        sitrepEpoch = nil
        zonesEpoch = nil
        startedEpoch = nil
        deferred?.cancel()
        deferred = nil
    }

    func warmSitrepOnce(store: AppStore, epoch: Int? = nil) async {
        let epoch = epoch ?? store.epoch
        guard store.isCurrent(epoch) else { return }
        // Pins first: ids the read model no longer carries were resolved while the
        // app was closed, so their images fall back into the LRU rather than
        // holding a permanent exemption.
        await ImageStore.shared.reconcile(activePins: Self.activePins(store))

        guard store.isCurrent(epoch), Prefs.shared.loadRemoteImages else { return }

        // ON SCREEN, AT ONCE: For-your-eyes before a single Reading image.
        let now = Self.split(Self.board(store)).now
        let done = Done()
        await warm(now.standing, epoch: epoch, store: store, done: done)
        await warm(now.reading, epoch: epoch, store: store, done: done)
        let warmed = done.newest

        guard store.isCurrent(epoch) else { return }
        // NOT AWAITED: awaiting a lower-priority task from this one would lift
        // it to this one's priority. What mostly keeps it off the first click
        // is WHEN it runs: the downloads and preparation inside the image and
        // thread caches run at their own priorities.
        deferred?.cancel()
        deferred = Task(priority: .background) { [weak self] in
            guard let self else { return }
            let pass = Pass(epoch: epoch, store: store)
            defer { pass.path.stop() }
            guard await self.waitForQuiet(epoch: epoch, store: store), self.allowed(pass) else { return }
            // THE BOARD AS IT IS NOW, not as it was at launch. On screen and
            // not warmed yet goes first: a row that came into view, one whose
            // thread the board now names newer mail in, one that failed at
            // launch (once ThreadPrefetch has stopped withholding it). Mail
            // resolved meanwhile is not pinned back: each thread is checked
            // against the board again before it goes, each message against
            // what is resolved. For-your-eyes before Reading, as at launch.
            let named = Self.named(store)
            let held = Set(warmed.filter { named[$0.key, default: 0] <= $0.value }.keys)
            let live = Self.split(Self.board(store), skipping: held)
            await self.warmLater(live.now.standing, pass)
            await self.warmLater(live.now.reading, pass)
            await self.warmLater(live.later.standing, pass)
            // The Reading cards behind the expander resolve their heroes when
            // they mount; resolving them here, four at a time, means an expand
            // finds them ready.
            await withBoundedTaskGroup(width: Self.warmWidth, over: live.later.heroes) { threadId in
                await self.resolveHero(threadId, pass)
            }
            await self.warmLater(live.later.reading, pass)
            // LAST, the rows on screen again, mostly cache hits: they end the
            // pass as the newest bodies in ThreadPrefetch's LRU, which the
            // pass has just filed hundreds behind, and a row that failed at
            // launch gets the try its withheld minute kept from it above.
            let screen = Self.split(Self.board(store)).now
            await self.warmLater(screen.standing, pass)
            await self.warmLater(screen.reading, pass)
        }
    }

    /// One deferred pass: the world it is for, the network path it watches,
    /// and whether a check has stopped it. A stop is for good: a constraint
    /// that lifts a moment later does not bring the pass back.
    @MainActor
    private final class Pass {
        let epoch: Int
        let store: AppStore
        let path = PathWatch()
        var stopped = false

        init(epoch: Int, store: AppStore) {
            self.epoch = epoch
            self.store = store
        }
    }

    /// The two lists as the board draws them: For-your-eyes in order, and the
    /// Reading zone pruned of mail already resolved here.
    private static func board(_ store: AppStore) -> (standing: [String], reading: [String]) {
        (store.sitrep.standing.map(\.thread_id), store.readingOnBoard.map(\.latestThreadId))
    }

    private struct Lists {
        var standing: [String] = []
        var reading: [String] = []
        var heroes: [String] = []
    }

    /// Each list split at its fold BEFORE the two are deduped, so a card on
    /// screen whose thread is also a For-your-eyes row further down is warmed
    /// now, as the card it is; then each thread once, in the order it goes.
    private static func split(
        _ board: (standing: [String], reading: [String]), skipping warmed: Set<String> = []
    ) -> (now: Lists, later: Lists) {
        var seen = warmed
        func unseen(_ ids: ArraySlice<String>) -> [String] {
            ids.filter { !$0.isEmpty && seen.insert($0).inserted }
        }
        var now = Lists()
        now.standing = unseen(board.standing.prefix(BoardFold.forYourEyes))
        now.reading = unseen(board.reading.prefix(BoardFold.reading))
        var later = Lists()
        later.standing = unseen(board.standing.dropFirst(BoardFold.forYourEyes))
        later.reading = unseen(board.reading.dropFirst(BoardFold.reading))
        later.heroes = board.reading.dropFirst(BoardFold.reading).filter { !$0.isEmpty }
        return (now, later)
    }

    /// The newest message the board names in each of its threads.
    private static func named(_ store: AppStore) -> [String: Int] {
        var named: [String: Int] = [:]
        let updates = store.sitrep.standing + store.readingOnBoard.flatMap(\.items)
        for update in updates {
            named[update.thread_id] = max(named[update.thread_id] ?? update.id, update.id)
        }
        return named
    }

    /// The threads the visible pass warmed all the way through, each with the
    /// newest message it held.
    private final class Done: @unchecked Sendable {
        var newest: [String: Int] = [:]
    }

    /// Whether the person is busy in a way the deferred half waits out: a
    /// reader up (a thread, or a scheduled send's preview, which takes the
    /// same slot), or on the phone, the app in the background, where a pass
    /// would be suspended mid-fetch. Not the reader's flight: `openThread`
    /// sets it with `threadId`, and `closeThread` leaves the last email's
    /// departure where it is.
    private static func busy(_ store: AppStore) -> Bool {
        if store.threadId != nil || store.scheduledPreviewRow != nil {
            return true
        }
        #if os(iOS)
            return !Platform.isAppOnScreen
        #else
            return false
        #endif
    }

    /// The visible pass: bounded, ungated but for the account and the pref.
    private func warm(_ threadIds: [String], epoch: Int, store: AppStore, done: Done) async {
        await withBoundedTaskGroup(width: Self.warmWidth, over: threadIds) { threadId in
            let newest = await self.warmThread(
                threadId, store: store, priority: .utility,
                mayPin: { store.isCurrent(epoch) && Prefs.shared.loadRemoteImages })
            if let newest { await MainActor.run { done.newest[threadId] = newest } }
        }
    }

    /// The deferred half's threads, `warmWidth` at a time, each asking
    /// `mayGo` before it starts and again before each message's images, so
    /// a reader opened mid-thread holds the rest of it too.
    private func warmLater(_ threadIds: [String], _ pass: Pass) async {
        await withBoundedTaskGroup(width: Self.warmWidth, over: threadIds) { threadId in
            await self.warmDeferred(threadId, pass)
        }
    }

    private func warmDeferred(_ threadId: String, _ pass: Pass) async {
        await warmThread(
            threadId, store: pass.store, priority: .background,
            mayPin: { [weak self] in await self?.mayGo(threadId, pass) ?? false })
    }

    /// One hero behind the Reading expander, asked the same as a thread.
    private func resolveHero(_ threadId: String, _ pass: Pass) async {
        guard await mayGo(threadId, pass), !ThreadPrefetch.shared.isWithheld(threadId) else { return }
        await HeroCache.shared.resolve(threadId)
    }

    /// Wait until nobody has been busy for `deferral.settle`, counted in
    /// `deferral.sample`s; while somebody is, look again every
    /// `deferral.recheck`. False for a world gone.
    private func waitForQuiet(epoch: Int, store: AppStore) async -> Bool {
        var quiet = Duration.zero
        while true {
            guard !Task.isCancelled, store.isCurrent(epoch) else { return false }
            if Self.busy(store) {
                quiet = .zero
                try? await Task.sleep(for: deferral.recheck)
            } else if quiet >= deferral.settle {
                return true
            } else {
                try? await Task.sleep(for: deferral.sample)
                quiet += deferral.sample
            }
        }
    }

    /// Asked before each step of the deferred half: wait out a busy person
    /// (and a fresh settle after), then go on only while the pass may, and
    /// for a thread still on the board.
    private func mayGo(_ threadId: String, _ pass: Pass) async -> Bool {
        if Self.busy(pass.store) {
            guard await waitForQuiet(epoch: pass.epoch, store: pass.store) else { return false }
        }
        guard allowed(pass) else { return false }
        return pass.store.sitrep.standing.contains { $0.thread_id == threadId }
            || pass.store.readingOnBoard.contains { $0.latestThreadId == threadId }
    }

    /// Whether the pass may go on: the same world, remote images still on,
    /// nothing in `constrained`. The first no stops the pass for good.
    private func allowed(_ pass: Pass) -> Bool {
        if !pass.stopped, !Task.isCancelled, pass.store.isCurrent(pass.epoch),
            Prefs.shared.loadRemoteImages, !constrained(pass.path)
        {
            return true
        }
        pass.stopped = true
        return false
    }

    /// Every message id the sitrep still surfaces, the reading zone included (their
    /// aggregated updates are the same message ids the bands use).
    private static func activePins(_ store: AppStore) -> Set<Int> {
        var ids = Set<Int>()
        for update in store.sitrep.standing + store.sitrep.new + store.sitrep.open {
            ids.insert(update.id)
        }
        for sender in store.zones.reading {
            for item in sender.items { ids.insert(item.id) }
        }
        return ids
    }

    /// Warm one thread's bodies. A visible row's thread is normally already
    /// cached, so this is usually a dictionary hit followed by the image
    /// fetches; kept for the warm lifetime either way. `mayPin` is asked before
    /// the fetch and before each message's images, and a no stops the thread:
    /// nothing for a world switched away from (the ids and the pins are the
    /// old account's), nothing once remote images are off. A message already
    /// resolved here is skipped, its pins having just been let go. Nothing at
    /// all for a thread ThreadPrefetch is withholding: it declined to be kept,
    /// or failed in the last minute. Each check runs on the main actor right
    /// before the step it lets through. The thread's newest message when it
    /// was warmed all the way through, nil when it was not.
    @discardableResult
    private func warmThread(
        _ threadId: String, store: AppStore, priority: TaskPriority,
        mayPin: @MainActor @Sendable () async -> Bool
    ) async -> Int? {
        guard !threadId.isEmpty, await mayPin(), !ThreadPrefetch.shared.isWithheld(threadId),
            let view = try? await ThreadPrefetch.shared.fetch(threadId, fresh: ThreadPrefetch.warmFresh)
        else { return nil }

        for message in view.messages {
            guard let html = message.html, !html.isEmpty, !store.resolvedIds.contains(message.id)
            else { continue }
            let prepared = await Self.prepared(html, message.attachmentList, priority: priority)
            guard !prepared.imageURLs.isEmpty else { continue }
            guard await mayPin() else { return nil }
            if store.resolvedIds.contains(message.id) { continue }
            await ImageStore.shared.warm(urls: prepared.imageURLs, pin: message.id)
        }
        return view.messages.lazy.map(\.id).max() ?? 0
    }

    /// The warm cache's answer, or a fresh pass OFF the main actor: an
    /// already-prepared body costs one dictionary probe, and one that is not
    /// prepared is never scanned on the main thread.
    ///
    /// ALWAYS the STRIPPED preparation, even for a known sender whose pixels the
    /// reader will keep: this runs before the mail is opened, and pre-fetching a
    /// tracking pixel would report an open that never happened. The known-sender
    /// body is prepared for real when the card renders it.
    private static func prepared(
        _ html: String, _ attachments: [Attachment], priority: TaskPriority
    ) async -> EmailWebView.Prepared {
        let key = EmailWebView.Prepared.cacheKey(html, false, attachments)
        if let warm = PreparedBodies.shared.get(key) { return warm }
        let made = await Task.detached(priority: priority) {
            EmailWebView.Prepared.make(from: html, allowTrackers: false, attachments: attachments)
        }.value
        PreparedBodies.shared.set(key, made)
        return made
    }
}

/// The network path the app is on: whether it costs the person something (a
/// phone's cellular data, a personal hotspot, Low Data Mode), and whether
/// there is one at all. Watches from its creation until `stop`; before the
/// monitor's first answer both say no.
final class PathWatch: @unchecked Sendable {
    private let monitor = NWPathMonitor()
    private let lock = NSLock()
    private var latest: NWPath?

    init() {
        monitor.pathUpdateHandler = { [weak self] path in self?.note(path) }
        monitor.start(queue: DispatchQueue(label: "passband.path-watch", qos: .utility))
    }

    private func note(_ path: NWPath) {
        lock.lock()
        latest = path
        lock.unlock()
    }

    private var path: NWPath? {
        lock.lock()
        defer { lock.unlock() }
        return latest
    }

    var isCostly: Bool { path.map { $0.isExpensive || $0.isConstrained } ?? false }
    var isOffline: Bool { path.map { $0.status != .satisfied } ?? false }

    func stop() { monitor.cancel() }
}
