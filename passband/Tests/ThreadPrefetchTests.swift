import Foundation

// Narrow stand-ins for rendering/network dependencies. Tests compile and exercise
// the real ThreadPrefetch and AsyncMemo; no API or web frame is involved.
struct ClientThreadView: Sendable {
    var cache_allowed: Bool? = true
    var messages: [ClientMessage] = []
    var marker: Int = 0
}
struct ClientMessage: Sendable {
    var id = 0
    var html: String?
    var allowsTrackers = false
    var attachmentList: [Int] = []
    func presented(original: Bool = false) -> ClientMessage { self }
}
@MainActor final class APIClient {
    static let shared = APIClient()
    func getThread(_ id: String) async throws -> ClientThreadView { fatalError("inject loader") }
}
enum EmailWebView {
    struct Prepared: Sendable {
        static func cacheKey(_ html: String, _ allow: Bool, _ parts: [Int]) -> Int { 0 }
        static func make(from: String, allowTrackers: Bool, attachments: [Int]) -> Self { Self() }
    }
}
enum ThreadStyle: CaseIterable {
    case classic, bubbles
    func frameKey(_ id: Int) -> String { "\(self):\(id)" }
    func frameKey(_ id: Int, translated: Bool) -> String { frameKey(id) + (translated ? ".tr" : "") }
}

@MainActor private final class Requests {
    var pending: [CheckedContinuation<ClientThreadView, Error>] = []
    func load(_ id: String) async throws -> ClientThreadView {
        try await withCheckedThrowingContinuation { pending.append($0) }
    }
}

/// Loads keyed by thread, for the checks that care which thread was asked for.
@MainActor private final class Loads {
    var asked: [String] = []
    private var waiting: [String: [CheckedContinuation<ClientThreadView, Error>]] = [:]
    func load(_ id: String) async throws -> ClientThreadView {
        asked.append(id)
        return try await withCheckedThrowingContinuation { waiting[id, default: []].append($0) }
    }
    /// Answers the OLDEST load still waiting for this thread.
    func answer(_ id: String, _ view: ClientThreadView) {
        waiting[id]!.removeFirst().resume(returning: view)
    }
    func fail(_ id: String, _ error: Error) {
        waiting[id]!.removeFirst().resume(throwing: error)
    }
}

@MainActor private final class TestClock {
    var now = Date(timeIntervalSince1970: 1_000_000)
}

private func thread(_ ids: [Int], marker: Int, allowed: Bool = true) -> ClientThreadView {
    ClientThreadView(cache_allowed: allowed, messages: ids.map { ClientMessage(id: $0) }, marker: marker)
}

@main struct ThreadPrefetchTests {
    @MainActor static func spin() async { for _ in 0..<50 { await Task.yield() } }
    @MainActor static func main() async throws {
        let requests = Requests()
        let cache = ThreadPrefetch(loader: requests.load)
        cache.prefetch("same")
        await spin()
        let hero = Task { try await cache.fetch("same", fresh: 600) }
        let peek = Task { try await cache.fetch("same") }
        await spin()
        precondition(requests.pending.count == 1, "preload, hero and peek must share one GET")
        requests.pending[0].resume(returning: ClientThreadView(marker: 1))
        let heroView = try await hero.value
        let peekView = try await peek.value
        precondition(heroView.marker == 1 && peekView.marker == 1)
        _ = try await cache.fetch("same")
        precondition(requests.pending.count == 1, "successful fetch must populate TTL cache")

        let old = Task { try await cache.fetch("switch") }
        await spin()
        cache.wipe()
        let new = Task { try await cache.fetch("switch") }
        await spin()
        precondition(requests.pending.count == 3)
        requests.pending[1].resume(returning: ClientThreadView(marker: 2))
        do { _ = try await old.value; fatalError("old-account result escaped") }
        catch is CancellationError {}
        let join = Task { try await cache.fetch("switch") }
        await spin()
        precondition(requests.pending.count == 3, "old completion must not remove new task")
        requests.pending[2].resume(returning: ClientThreadView(marker: 3))
        _ = try await new.value
        let joined = try await join.value
        precondition(joined.marker == 3 && cache.cached("switch")?.marker == 3)

        let failed = Task { try await cache.fetch("retry") }
        await spin()
        requests.pending[3].resume(throwing: URLError(.notConnectedToInternet))
        do { _ = try await failed.value; fatalError("expected failure") }
        catch is URLError {}
        let retry = Task { try await cache.fetch("retry") }
        await spin()
        precondition(requests.pending.count == 5, "failed fetch must allow another attempt")
        requests.pending[4].resume(returning: ClientThreadView(cache_allowed: false))
        _ = try await retry.value
        precondition(cache.cached("retry") == nil, "restricted response must not be cached")

        try await warmPaths()
        print("ThreadPrefetch tests passed")
    }

    /// The warm's lifetime and what cuts it short (`retire`), what is never
    /// replaced by an older answer, the threads the daemon declines or fails
    /// to answer for, and what a fetch in flight counts for, on a clock the
    /// suite moves.
    @MainActor static func warmPaths() async throws {
        let clock = TestClock()
        let loads = Loads()
        let cache = ThreadPrefetch(loader: loads.load, now: { clock.now })

        cache.warm(["a", "b", "a", ""], spacing: .milliseconds(1))
        await spin()
        precondition(loads.asked.sorted() == ["a", "b"], "a warm asks once per thread, never for an empty id: \(loads.asked)")
        loads.answer("a", thread([1, 2], marker: 10))
        loads.answer("b", thread([5], marker: 20))
        await spin()
        precondition(cache.cached("a")?.marker == 10 && cache.cached("b")?.marker == 20)
        precondition(!cache.isAged("a"), "a copy inside the minute is not the reader's to check")

        // Past the old 60 s a warmed thread is still served, for the reader
        // to check, and neither a warm nor a hover asks for it again.
        clock.now += 61
        precondition(cache.cached("a")?.marker == 10, "a warmed thread outlives the 60 s default")
        precondition(cache.isAged("a"), "and is the reader's to check once it is past it")
        cache.warm(["a", "b"])
        cache.prefetch("a")
        await spin()
        precondition(loads.asked.count == 2, "a warm or a hover inside the warm lifetime asks nothing: \(loads.asked)")

        // A row naming a message the copy holds, or an older one, is not news;
        // a newer one drops that thread's copy and nothing else.
        cache.retire([("a", 2), ("a", 1), ("b", 3)])
        precondition(cache.cached("a")?.marker == 10 && cache.cached("b")?.marker == 20, "an old message is not news")
        cache.retire([("a", 3)])
        precondition(cache.cached("a") == nil, "a row naming a newer message drops the copy")
        precondition(cache.cached("b")?.marker == 20, "and only that thread's copy")
        cache.warm(["a", "b"])
        await spin()
        precondition(loads.asked.count == 3 && loads.asked[2] == "a", "the next warm asks for that thread alone: \(loads.asked)")

        // A message named while the fetch is out: the answer that predates it
        // goes to the caller and is not filed, so the next warm asks again;
        // one that holds it is filed.
        cache.retire([("a", 4)])
        loads.answer("a", thread([1, 2, 3], marker: 11))
        await spin()
        precondition(cache.cached("a") == nil, "an answer read before a message a row named is not filed")
        cache.warm(["a"])
        await spin()
        precondition(loads.asked.count == 4, "so the next warm asks again: \(loads.asked)")
        cache.retire([("a", 4)])
        loads.answer("a", thread([1, 2, 3, 4], marker: 12))
        await spin()
        precondition(cache.cached("a")?.marker == 12, "an answer holding the named message is filed")

        // Five minutes on, a warmed thread is neither served nor skipped.
        clock.now += 240
        precondition(cache.cached("b") == nil, "five minutes on, a warmed thread is not served")
        cache.warm(["b"])
        await spin()
        precondition(loads.asked.count == 5 && loads.asked.last == "b", "and the warm asks again: \(loads.asked)")

        // An answer whose fetch began before a newer copy was filed (the
        // reader's refetch after a send) does not replace it.
        cache.note("b", thread([5, 6], marker: 21))
        loads.answer("b", thread([5], marker: 22))
        await spin()
        precondition(cache.cached("b")?.marker == 21, "an older answer does not replace a newer copy filed while it was out")
        // Nor one holding the same mail, filed later with newer marks (the
        // status a send or a done left).
        cache.warm(["q"])
        await spin()
        cache.note("q", thread([200], marker: 201))
        loads.answer("q", thread([200], marker: 202))
        await spin()
        precondition(cache.cached("q")?.marker == 201, "an older answer with the same mail does not replace a later copy")

        // A thread the daemon declines to let us keep: never served; marked
        // for the minute whatever the caller asked (a receipt asks until
        // midnight) and not asked for inside it; still fetched for a caller
        // that needs the view; asked for again once a newer message lands.
        cache.prefetch("r", fresh: 3600)
        await spin()
        precondition(loads.asked.count == 7 && loads.asked.last == "r")
        loads.answer("r", thread([70], marker: 80, allowed: false))
        await spin()
        precondition(cache.cached("r") == nil, "a declined thread is never served")
        precondition(cache.isWithheld("r"), "and is left alone, which the frame prewarmer asks too")
        cache.warm(["r"])
        cache.prefetch("r", fresh: 3600)
        await spin()
        precondition(loads.asked.count == 7, "a declined thread is not asked for on every tick: \(loads.asked)")
        clock.now += 61
        precondition(!cache.isWithheld("r"))
        cache.prefetch("r", fresh: 3600)
        await spin()
        precondition(loads.asked.count == 8, "a minute on it is, whatever the caller asked for: \(loads.asked)")
        // The daemon's later word stands: an allowed answer whose fetch began
        // before a declined one was filed does not cache the thread.
        cache.note("r", thread([70], marker: 81, allowed: false))
        loads.answer("r", thread([70], marker: 82))
        await spin()
        precondition(cache.cached("r") == nil, "an older answer does not overrule a later decline")
        let needed = Task { try await cache.fetch("r") }
        await spin()
        precondition(loads.asked.count == 9, "a caller that needs the view still gets one")
        loads.answer("r", thread([70], marker: 83, allowed: false))
        let got = try await needed.value
        precondition(got.marker == 83 && cache.cached("r") == nil)
        cache.retire([("r", 71)])
        cache.warm(["r"])
        await spin()
        precondition(loads.asked.count == 10, "a newer message is worth asking about, declined or not")
        loads.answer("r", thread([70, 71], marker: 84))
        await spin()
        precondition(cache.cached("r")?.marker == 84)

        // A thread that will not load is not asked for on the next tick, even
        // with its row naming a message every tick, and is a minute on.
        cache.warm(["f"])
        await spin()
        loads.fail("f", URLError(.notConnectedToInternet))
        await spin()
        cache.retire([("f", 95)])
        cache.warm(["f"])
        await spin()
        precondition(loads.asked.count == 11, "a failed thread is not asked for on the next tick: \(loads.asked)")
        clock.now += 61
        cache.warm(["f"])
        await spin()
        precondition(loads.asked.count == 12 && loads.asked.last == "f", "a minute on, it is: \(loads.asked)")
        loads.answer("f", thread([95], marker: 96))
        await spin()
        precondition(cache.cached("f")?.marker == 96)
        // A failure leaves a copy where it is: past the warm's five minutes,
        // a caller asking for ten (the hero) is still served it.
        clock.now += 301
        cache.warm(["f"])
        await spin()
        precondition(loads.asked.count == 13, "a lapsed copy is asked for again: \(loads.asked)")
        loads.fail("f", URLError(.timedOut))
        await spin()
        let hero = try await cache.fetch("f", fresh: 600)
        precondition(hero.marker == 96 && loads.asked.count == 13, "a failure does not take a copy a longer-lived caller can use")

        // A fetch out is joined rather than asked for again, the joiner's
        // longer lifetime is the one filed, and `immediate` counts rows: the
        // row after the first trickles.
        cache.prefetch("g")
        await spin()
        cache.warm(["g", "h"], immediate: 1, spacing: .milliseconds(300))
        await spin()
        precondition(loads.asked.filter { $0 == "g" }.count == 1, "a thread with a fetch out is joined, not asked for again: \(loads.asked)")
        precondition(!loads.asked.contains("h"), "the second row is not asked for at once: \(loads.asked)")
        try await eventually { loads.asked.contains("h") }
        precondition(loads.asked.count == 15, "it trickles in after the spacing: \(loads.asked)")
        loads.answer("g", thread([100], marker: 110))
        loads.answer("h", thread([120], marker: 130))
        await spin()
        clock.now += 61
        precondition(cache.cached("g")?.marker == 110, "the warm that joined a hover's fetch files the warm's lifetime")

        // A wipe forgets what an in-flight fetch was told, and the answer to
        // that fetch, landing after it, files nothing.
        cache.warm(["e"])
        await spin()
        cache.retire([("e", 99)])
        cache.wipe()
        loads.answer("e", thread([9], marker: 49))
        await spin()
        precondition(cache.cached("e") == nil, "an answer for the old account is not filed")
        let after = Task { try await cache.fetch("e") }
        await spin()
        precondition(loads.asked.count == 17, "a wipe starts the thread over: \(loads.asked)")
        loads.answer("e", thread([9], marker: 50))
        _ = try await after.value
        precondition(cache.cached("e")?.marker == 50, "a name heard for the old account does not hold back the new one")
        cache.drop("e")
        precondition(cache.cached("e") == nil, "a thread the daemon no longer serves is forgotten")
        precondition(cache.isWithheld("e"), "and left alone for the minute")
        // A warm's answer from before the 404 is not filed after it.
        cache.warm(["d"])
        await spin()
        cache.drop("d")
        loads.answer("d", thread([300], marker: 301))
        await spin()
        precondition(loads.asked.count == 18 && cache.cached("d") == nil, "an answer from before a drop is not filed: \(loads.asked)")
    }

    /// For what the trickle's real sleeps bring about: polls, and fails the
    /// suite after `seconds` rather than hanging it.
    @MainActor static func eventually(_ seconds: Double = 5, _ condition: () -> Bool) async throws {
        let deadline = Date().addingTimeInterval(seconds)
        while !condition() {
            precondition(Date() < deadline, "timed out waiting for the trickle")
            try await Task.sleep(for: .milliseconds(5))
        }
    }
}
