// Exercise the production poller with an intentionally cancellation-oblivious
// transport: the old account may answer after fixture warmup has begun. Then
// the loop's cadence against fake presence, on the production schedule run
// twenty times faster (`PollTiming`), so the whole of it takes about twenty
// seconds.
import Foundation

@main
struct SitrepPollerTests {
    @MainActor
    static func main() async {
        let poller = SitrepPoller.shared
        let api = APIClient.shared
        let store = AppStore.shared
        // One band row on both answers: the stale one must retire nothing.
        api.feed = Feed(items: [FeedItem(row: AttentionUpdate(id: 7, thread_id: "t7", senderString: "a"))])
        let old = Task { await poller.pull() }
        await until { api.requests == 1 }

        poller.stop()
        store.epoch += 1
        let fresh = Task { await poller.pull() }
        await until { api.requests == 2 }
        expect(api.pending.count == 2, "fixtures start before the old request finishes")

        api.finish(1)
        _ = await old.value
        expect(store.stats == nil, "stale response must not populate fixtures")
        expect(ThreadPrefetch.shared.retired.isEmpty, "a stale answer's rows retire nothing")
        expect(Badge.refreshes == 0, "stale response must not update badge")
        expect(store.lastRefresh == nil, "stale response must not mark a refresh")
        expect(store.statsEpoch == nil, "stale response must not claim the stats for any epoch")

        let joined = Task { await poller.pull() }
        for _ in 0..<100 { await Task.yield() }
        expect(api.requests == 2, "old completion must not clear the new in-flight task")
        api.finish(2)
        _ = await (fresh.value, joined.value)
        expect(store.stats?.total == 2, "fresh fixture response reaches the board")
        expect(ThreadPrefetch.shared.retired.map { $0.0 } == ["t7"] && ThreadPrefetch.shared.retired.map { $0.1 } == [7],
               "a pull hands every band row to the thread cache: \(ThreadPrefetch.shared.retired)")
        api.feed = Feed()
        expect(store.statsEpoch == store.epoch, "the stats are stamped with the epoch they were asked under")
        expect(Badge.refreshes == 1, "only the current request has side effects")

        // stop() itself protects the store, even without an epoch bump.
        let canceled = Task { await poller.pull() }
        await until { api.requests == 3 }
        poller.stop()
        api.finish(3)
        _ = await canceled.value
        expect(store.stats?.total == 2, "canceled response is ignored without an epoch change")

        let failed = Task { await poller.pull() }
        await until { api.requests == 4 }
        poller.stop()
        api.fail(4)
        _ = await failed.value
        expect(store.refreshError == nil, "canceled errors cannot poison the new mailbox")

        store.connStatus = .loading
        _ = await poller.pull()
        expect(api.requests == 4, "transition loading gate prevents requests")

        // THE CADENCE, from what the person can see.
        expect(PollCadence(active: true, onScreen: true) == .foreground, "frontmost with a window is the foreground cadence")
        expect(PollCadence(active: false, onScreen: true) == .behind, "a window behind another app can still be glanced at")
        expect(PollCadence(active: true, onScreen: false) == .hidden, "frontmost with nothing on screen (a sleeping display) is hidden")
        expect(PollCadence(active: false, onScreen: false) == .hidden, "nothing on screen is hidden")
        let production = PollTiming()
        expect(production.interval(.foreground) == .seconds(10), "the foreground cadence is the ten seconds it always was")
        expect(production.interval(.foreground) < production.interval(.behind)
            && production.interval(.behind) < production.interval(.hidden), "less to see, slower")
        expect(production.backoffBase == 20 && production.backoffCap == 60, "the failure backoff is the one it always was")
        expect(Duration.milliseconds(1500).timeInterval == 1.5, "a duration in seconds")

        // THE BACKOFF's step without the sleep: the window doubles to the cap,
        // each delay jittered over its lower half, and a reset starts it over.
        var backoff = Backoff(base: 20, cap: 60)
        var delays: [TimeInterval] = []
        for _ in 0..<4 { delays.append(backoff.next()) }
        expect(zip(delays, [20.0, 40, 60, 60]).allSatisfy { $0 >= $1 / 2 && $0 <= $1 },
               "the window doubles to the cap, each delay in its lower half: \(delays)")
        backoff.reset()
        let first = backoff.next()
        expect(first >= 10 && first <= 20, "a reset starts at the base: \(first)")

        // THE LOOP, against fake presence, on the production schedule run
        // twenty times faster: a 500 ms tick, a 1.5 s behind cadence, a 15 s
        // hidden one, a backoff from 0.5 s. Every "not sooner" window is
        // measured from the moment the pull it follows was finished, which no
        // scheduling delay can make early, and every "sooner" check would time
        // out at the slower schedule it rules out.
        poller.timing = PollTiming(
            foreground: .milliseconds(500), behind: .milliseconds(1500), hidden: .seconds(15))
        store.connStatus = .connected
        poller.start()
        await until { api.requests == 5 }
        var finished = ContinuousClock.now
        api.finish(5)
        await until { poller.isNapping }
        expect(poller.cadence == .foreground, "a frontmost app starts at the foreground cadence")

        // Off screen: the next pull moves out to the hidden cadence at once,
        // past both the foreground tick and the behind cadence.
        Platform.isAppActive = false
        Platform.isAppOnScreen = false
        NotificationCenter.default.post(name: Platform.presenceNotifications[1], object: nil)
        await until { poller.cadence == .hidden }
        await holds(until: finished + .seconds(2), { api.requests == 5 }, "nothing on screen waits out the hidden cadence")

        // A live event long after the last pull pulls now...
        poller.noteLiveEvent()
        await within(1) { api.requests == 6 }
        finished = ContinuousClock.now
        api.finish(6)
        await until { poller.isNapping }
        // ...and right after one, waits out the ask floor (the behind cadence
        // while nothing is on screen), then pulls well before the hidden one.
        poller.noteLiveEvent()
        await holds(until: finished + .milliseconds(1300), { api.requests == 6 }, "events while hidden pull no faster than the behind cadence")
        await within(until: finished + .seconds(3)) { api.requests == 7 }
        finished = ContinuousClock.now
        api.finish(7)
        await until { poller.isNapping }

        // An event during a pull it may have missed (a manual refresh that
        // started before it) is answered by the next pull, not by that one.
        let manual = Task { await poller.pull() }
        await until { api.requests == 8 }
        poller.noteLiveEvent()
        finished = ContinuousClock.now
        api.finish(8)
        _ = await manual.value
        await holds(until: finished + .milliseconds(1300), { api.requests == 8 }, "the follow-up waits out the ask floor")
        await within(until: finished + .seconds(3)) { api.requests == 9 }
        finished = ContinuousClock.now
        api.finish(9)
        await until { poller.isNapping }

        // Back on screen behind another app: the behind cadence from the last
        // pull, well before the hidden one.
        Platform.isAppOnScreen = true
        NotificationCenter.default.post(name: Platform.presenceNotifications[2], object: nil)
        await until { poller.cadence == .behind }
        await holds(until: finished + .milliseconds(1300), { api.requests == 9 }, "behind waits out the behind cadence")
        await within(until: finished + .seconds(3)) { api.requests == 10 }
        finished = ContinuousClock.now
        api.finish(10)
        await until { poller.isNapping }

        // With a window on screen behind another app, a live event pulls one
        // foreground tick after the last pull, before the behind cadence.
        poller.noteLiveEvent()
        await holds(until: finished + .milliseconds(350), { api.requests == 10 }, "an event behind waits out one tick")
        await within(until: finished + .milliseconds(1200)) { api.requests == 11 }
        finished = ContinuousClock.now
        api.finish(11)
        await until { poller.isNapping }

        // Focus pulls at once, sooner than the schedule's next pull, and the
        // loop then runs at the foreground cadence from that pull.
        Platform.isAppActive = true
        NotificationCenter.default.post(name: Platform.didBecomeActiveNotification, object: nil)
        await until { api.requests == 12 }
        expect(poller.cadence == .foreground, "focus is the foreground cadence")
        await holds(for: .milliseconds(150), { api.requests == 12 }, "focus is one pull")
        finished = ContinuousClock.now
        api.finish(12)
        await holds(until: finished + .milliseconds(350), { api.requests == 12 }, "the loop waits a foreground tick after focus")
        await within(until: finished + .milliseconds(1200)) { api.requests == 13 }
        finished = ContinuousClock.now
        api.finish(13)
        await until { poller.isNapping }

        // A pull somebody else makes moves the loop's next pull behind it.
        let refresh = Task { await poller.pull() }
        await until { api.requests == 14 }
        try? await Task.sleep(for: .milliseconds(200))
        finished = ContinuousClock.now
        api.finish(14)
        _ = await refresh.value
        await holds(until: finished + .milliseconds(350), { api.requests == 14 }, "the loop waits a tick behind the other pull")
        await within(until: finished + .milliseconds(1200)) { api.requests == 15 }
        api.finish(15)
        await until { poller.isNapping }

        // A failure behind another app is retried no sooner than the behind
        // cadence (1.5 to 1.9 s here), not on the backoff's quicker first step
        // (0.5 to 1 s), and an event meanwhile does not cut it to the one-tick
        // ask floor: a failing daemon is never asked more often than a healthy
        // one, and the event stream can be up while the door sheds load.
        Platform.isAppActive = false
        Platform.isAppOnScreen = true
        NotificationCenter.default.post(name: Platform.presenceNotifications[2], object: nil)
        await until { poller.cadence == .behind }
        await within(3) { api.requests == 16 }
        finished = ContinuousClock.now
        api.fail(16)
        await until { poller.isNapping }
        poller.noteLiveEvent()
        await holds(until: finished + .milliseconds(1400), { api.requests == 16 }, "a failure waits out the cadence")
        await within(until: finished + .milliseconds(2500)) { api.requests == 17 }

        // With nothing on screen the retry waits out the hidden cadence, past
        // both the backoff (1 to 2 s now) and the behind cadence; a window
        // coming back brings it in to the behind cadence's, already passed.
        Platform.isAppOnScreen = false
        NotificationCenter.default.post(name: Platform.presenceNotifications[1], object: nil)
        await until { poller.cadence == .hidden }
        finished = ContinuousClock.now
        api.fail(17)
        await until { poller.isNapping }
        await holds(until: finished + .milliseconds(2500), { api.requests == 17 }, "a hidden failure waits out the hidden cadence")
        Platform.isAppOnScreen = true
        NotificationCenter.default.post(name: Platform.presenceNotifications[2], object: nil)
        await within(1) { api.requests == 18 }

        // A retry that comes due with the door not connected asks nothing of
        // the daemon and leaves the loop napping the cadence, rather than
        // spinning on a retry left due in the past. A spinning main actor
        // never reaches the watchdog's disarm, so it fails the suite rather
        // than hanging it.
        let watchdog = Watchdog(seconds: 10, "the main actor spun on a retry left due in the past")
        api.fail(18)
        store.connStatus = .loading
        await holds(for: .seconds(3), { api.requests == 18 }, "a disconnected door asks nothing")
        watchdog.disarm()
        expect(poller.isNapping, "the loop naps the cadence while the door is down")
        store.connStatus = .connected
        poller.noteLiveEvent()
        await within(3) { api.requests == 19 }
        api.finish(19)
        await until { poller.isNapping }

        // stop() ends the loop itself. Frontmost first, so a loop still
        // running would pull within half a second; then focus no longer
        // reaches it either.
        Platform.isAppActive = true
        Platform.isAppOnScreen = true
        NotificationCenter.default.post(name: Platform.didBecomeActiveNotification, object: nil)
        await until { api.requests == 20 }
        api.finish(20)
        await until { poller.isNapping }
        poller.stop()
        NotificationCenter.default.post(name: Platform.didBecomeActiveNotification, object: nil)
        await holds(for: .seconds(2), { api.requests == 20 }, "a stopped poller pulls nothing and hears no presence")
        print("SitrepPoller tests passed")
    }

    /// Wait on the real clock for `condition`, at most `seconds`. A miss is
    /// reported at the caller's line.
    @MainActor
    static func within(
        _ seconds: Double, file: StaticString = #fileID, line: UInt = #line,
        _ condition: () -> Bool
    ) async {
        await within(until: ContinuousClock.now + .seconds(seconds), file: file, line: line, condition)
    }

    /// Wait on the real clock for `condition`, until `deadline` at the latest.
    @MainActor
    static func within(
        until deadline: ContinuousClock.Instant, file: StaticString = #fileID, line: UInt = #line,
        _ condition: () -> Bool
    ) async {
        while ContinuousClock.now < deadline {
            if condition() { return }
            try? await Task.sleep(for: .milliseconds(5))
        }
        if !condition() { fatalError("Not by the deadline", file: file, line: line) }
    }

    /// `condition` holds for all of `duration` on the real clock.
    @MainActor
    static func holds(
        for duration: Duration, _ condition: () -> Bool, _ message: String,
        file: StaticString = #fileID, line: UInt = #line
    ) async {
        await holds(until: ContinuousClock.now + duration, condition, message, file: file, line: line)
    }

    /// `condition` holds on the real clock until `deadline`.
    @MainActor
    static func holds(
        until deadline: ContinuousClock.Instant, _ condition: () -> Bool, _ message: String,
        file: StaticString = #fileID, line: UInt = #line
    ) async {
        while ContinuousClock.now < deadline {
            if !condition() { fatalError(message, file: file, line: line) }
            try? await Task.sleep(for: .milliseconds(5))
        }
        if !condition() { fatalError(message, file: file, line: line) }
    }

    @MainActor
    static func until(_ condition: () -> Bool) async {
        for _ in 0..<10_000 {
            if condition() { return }
            await Task.yield()
        }
        fatalError("Timed out waiting for fake transport")
    }

    static func expect(_ condition: @autoclosure () -> Bool, _ message: String) {
        if !condition() { fatalError(message) }
    }
}

/// Fails the suite from another thread if it is not disarmed in time: the one
/// way to report a main actor that never yields, which no check on the main
/// actor can see.
final class Watchdog: @unchecked Sendable {
    private let lock = NSLock()
    private var disarmed = false

    init(seconds: UInt32, _ message: String) {
        Thread.detachNewThread { [self] in
            sleep(seconds)
            lock.lock()
            let done = disarmed
            lock.unlock()
            if !done { fatalError(message) }
        }
    }

    func disarm() {
        lock.lock()
        disarmed = true
        lock.unlock()
    }
}

// Narrow collaborators; SitrepPoller.swift and Concurrency.swift are unmodified
// production sources in this executable. No network, credentials, or app host.
enum NeedToday { static let bandLimit = 100 }
enum Platform {
    static let didBecomeActiveNotification = Notification.Name("test.focus")
    static let presenceNotifications = [
        didBecomeActiveNotification, Notification.Name("test.resign"),
        Notification.Name("test.occlusion"),
    ]
    @MainActor static var isAppActive = true
    @MainActor static var isAppOnScreen = true
    @MainActor static func presence(after name: Notification.Name) -> (active: Bool, onScreen: Bool) {
        (isAppActive, isAppOnScreen)
    }
}
enum ConnectionStatus { case connected, loading }
enum Band { case standing, new, open }
struct UpdatesParams: Sendable { let band: Band; let limit: Int }
struct AttentionUpdate: Equatable, Sendable {
    var id: Int; var thread_id: String; var senderString: String
}
struct Feed: Sendable { var items: [FeedItem] = []; var total_count: Int? = 0 }
struct FeedItem: Sendable { var row: AttentionUpdate }
struct Updates: Sendable { var items: [AttentionUpdate] = [] }
struct Stats: Equatable, Sendable {
    var total: Int
    /// Optional, as on the wire: a daemon too old to name its mailbox sends
    /// nothing, and the seed below must compile against that shape.
    var account_email: String? = "practice@example.test"
    var account_given_name: String?
    var account_photo: String?
    var sealed = 0
    var bands = Bands()
    var tier_counts: [String: Int] = [:]
    /// Optional, as on the wire: present only while the daemon is walking a
    /// known amount of mail. The poller reads it for `catchup_completed`.
    var catch_up: CatchUpProgress?
    struct Bands: Equatable, Sendable { var standing = 0; var new = 0; var open = 0 }
}
struct CatchUpProgress: Equatable, Sendable {
    var done: Int
    var total: Int
}
struct SitrepData: Equatable {
    var standing: [AttentionUpdate] = []
    var new: [AttentionUpdate] = []
    var open: [AttentionUpdate] = []
    var sealed: [Int] = []
    var totalCount: Int?
}
struct OpenThreadSummary { var threadId: String; var newestMessageId: Int }
enum ErrorKind { case unknown }
struct APIError: Error { var message: String; var kind: ErrorKind }
struct RefreshError { var message: String; var kind: ErrorKind }
@MainActor final class AppStore {
    var stats: Stats?
    var statsEpoch: Int?
    static let shared = AppStore()
    var connStatus = ConnectionStatus.connected
    var epoch = 0
    var sitrep = SitrepData()
    var currentThreadSummary: OpenThreadSummary?
    var refreshError: RefreshError?
    var lastRefresh: Date?
    var selectedId: Int?
    var orderedIds: [Int] { [] }
    func isCurrent(_ e: Int) -> Bool { epoch == e }
    func refreshZones(force: Bool = false) async {}
    func noteBoardConfirmed() {}
    func notePullLanded(epoch: Int) {}
    func pollScheduled() async {}
    func noteThreadArrival(thread: String, message: Int, sender: String) {}
}
@MainActor final class APIClient {
    static let shared = APIClient()
    var requests = 0
    var pending: [Int: CheckedContinuation<Stats, Error>] = [:]
    func getStats() async throws -> Stats {
        requests += 1
        let id = requests
        return try await withCheckedThrowingContinuation { pending[id] = $0 }
    }
    func finish(_ id: Int) { pending.removeValue(forKey: id)!.resume(returning: Stats(total: id)) }
    func fail(_ id: Int) { pending.removeValue(forKey: id)!.resume(throwing: APIError(message: "old failure", kind: .unknown)) }
    var feed = Feed()
    func getFeed(destination: String, limit: Int) async throws -> Feed { feed }
    func getUpdates(_ params: UpdatesParams) async throws -> Updates { Updates() }
    func listSealed() async throws -> [Int] { [] }
    func refreshMail() async throws {}
}
@MainActor enum Badge {
    static var refreshes = 0
    static func set(_ count: Int) { refreshes += 1 }
    static func refresh(_ rows: [AttentionUpdate]) { refreshes += 1 }
}
@MainActor final class Prefs {
    static let shared = Prefs()
    func seedUserName(fromGivenName: String?, email: String?) {}
}
@MainActor final class AccountPhotos {
    static let shared = AccountPhotos()
    func noteActive(tag: String?) {}
}
enum Analytics {
    static func capture(_ event: String) {}
    static func capture(_ event: String, _ properties: [String: Int]) {}
    static func daily(_ event: String, _ properties: [String: Int]) {}
}
@MainActor final class ThreadPrefetch {
    static let shared = ThreadPrefetch()
    var retired: [(String, Int)] = []
    func retire(_ rows: some Sequence<(String, Int)>) { retired.append(contentsOf: rows) }
}
@MainActor final class ImageWarmer {
    static let shared = ImageWarmer()
    func noteSitrepLanded() {}
}

/// The verdict ledger's view of sealed lists (Lib/AuthCode.swift): numbered
/// fetches, stale lists dropped. Every list is fresh here.
@MainActor final class AuthDecisions {
    static let shared = AuthDecisions()
    func beginListFetch() -> Int { 0 }
    func isFresh(_ fetch: Int) -> Bool { true }
    func listed(_ rows: [Int], fetch: Int) {}
}
