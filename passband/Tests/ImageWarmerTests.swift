// The launch image warm, compiled from the real ImageWarmer.swift against
// stand-ins for the store, the caches and the image fetcher: what goes at once,
// what waits for a quiet board, and what stops it.
import Foundation

struct AttentionUpdate: Sendable {
    var id: Int
    var thread_id: String
}
struct ReadingSender: Sendable {
    var latestThreadId: String
    var items: [AttentionUpdate]
}
struct SitrepData {
    var standing: [AttentionUpdate] = []
    var new: [AttentionUpdate] = []
    var open: [AttentionUpdate] = []
}
struct Zones { var reading: [ReadingSender] = [] }

@MainActor final class AppStore {
    static let shared = AppStore()
    var sitrep = SitrepData()
    var zones = Zones()
    var resolvedIds: Set<Int> = []
    /// As Reading.prune: a sender keeps its card while any of its mail is
    /// unresolved, pointed at the newest that is. No re-sort: the warm
    /// follows whatever order the store gives it.
    var readingOnBoard: [ReadingSender] {
        zones.reading.compactMap { sender in
            let live = sender.items.filter { !resolvedIds.contains($0.id) }
            guard let newest = live.first else { return nil }
            var out = sender
            out.items = live
            out.latestThreadId = newest.thread_id
            return out
        }
    }
    var epoch = 0
    var threadId: String?
    var scheduledPreviewRow: Int?
    func isCurrent(_ e: Int) -> Bool { epoch == e }
}

struct Attachment: Sendable {}
struct ClientMessage: Sendable {
    var id: Int
    var html: String?
    var attachmentList: [Attachment] = []
}
struct ClientThreadView: Sendable { var messages: [ClientMessage] }

@MainActor final class ThreadPrefetch {
    static let shared = ThreadPrefetch()
    static let warmFresh: TimeInterval = 300
    var asked: [String] = []
    var lifetimes: Set<TimeInterval?> = []
    /// As the real one: a failed fetch is withheld (for its minute, which
    /// the suite ends by hand).
    var withheld: Set<String> = []
    var failing: Set<String> = []
    /// Threads with an older message ahead of the one that names them.
    var older: Set<String> = []
    var delay: Duration = .zero
    func isWithheld(_ threadId: String) -> Bool { withheld.contains(threadId) }
    /// One message per thread, with an id that names it, so a pin can be
    /// traced back to the thread it was for. Handed out in order, from a
    /// range no other id in the suite uses.
    private static var ids: [String: Int] = [:]
    static func messageId(_ threadId: String) -> Int {
        if let id = ids[threadId] { return id }
        let id = 10_000 + ids.count
        ids[threadId] = id
        return id
    }
    func fetch(_ threadId: String, fresh: TimeInterval? = nil) async throws -> ClientThreadView {
        lifetimes.insert(fresh)
        asked.append(threadId)
        if delay > .zero { try? await Task.sleep(for: delay) }
        if failing.contains(threadId) {
            withheld.insert(threadId)
            throw URLError(.timedOut)
        }
        let names = (older.contains(threadId) ? [threadId + "^"] : []) + [threadId]
        return ClientThreadView(messages: names.map {
            ClientMessage(id: Self.messageId($0), html: "<img src=\"\($0)\">")
        })
    }
}

enum EmailWebView {
    struct Prepared: Sendable {
        var imageURLs: [String]
        static func cacheKey(_ html: String, _ allow: Bool, _ parts: [Attachment]) -> Int { html.hashValue }
        static func make(from html: String, allowTrackers: Bool, attachments: [Attachment]) -> Self {
            Self(imageURLs: [html])
        }
    }
}
final class PreparedBodies: @unchecked Sendable {
    static let shared = PreparedBodies()
    func get(_ key: Int) -> EmailWebView.Prepared? { nil }
    func set(_ key: Int, _ prepared: EmailWebView.Prepared) {}
}

@MainActor final class ImageStore {
    static let shared = ImageStore()
    var reconciled = 0
    var pins: [Int] = []
    func reconcile(activePins: Set<Int>) async { reconciled += 1 }
    func warm(urls: [String], pin messageId: Int?) async { if let messageId { pins.append(messageId) } }
}
@MainActor final class Prefs {
    static let shared = Prefs()
    var loadRemoteImages = true
}
@MainActor final class HeroCache {
    static let shared = HeroCache()
    var resolved: [String] = []
    @discardableResult
    func resolve(_ threadId: String) async -> Bool? {
        resolved.append(threadId)
        return nil
    }
}

@MainActor final class Flag { var on = false }

@main
struct ImageWarmerTests {
    @MainActor static func expect(_ condition: @autoclosure () -> Bool, _ message: String) {
        guard condition() else {
            print("FAIL: \(message)")
            exit(1)
        }
    }

    /// For what the deferred half's real sleeps bring about: polls, and fails
    /// the suite after `seconds` rather than hanging it.
    @MainActor static func eventually(_ seconds: Double = 5, _ condition: () -> Bool) async {
        let deadline = Date().addingTimeInterval(seconds)
        while !condition() {
            expect(Date() < deadline, "timed out")
            try? await Task.sleep(for: .milliseconds(10))
        }
    }

    @MainActor static func main() async {
        let store = AppStore.shared
        let warmer = ImageWarmer.shared
        let cache = ThreadPrefetch.shared
        warmer.deferral = ImageWarmer.Deferral(
            settle: .milliseconds(300), sample: .milliseconds(20), recheck: .milliseconds(50))
        let costly = Flag()
        warmer.constrained = { _ in costly.on }

        // Thirty For-your-eyes rows and forty Reading senders. Card 2 is the
        // thread of For-your-eyes row 3, on screen in both; card 0 is the
        // thread of row 20, a card on screen whose row is below the fold.
        let standing = (0..<30).map { AttentionUpdate(id: $0, thread_id: "s\($0)") }
        var reading = (0..<40).map {
            ReadingSender(latestThreadId: "r\($0)", items: [AttentionUpdate(id: 100 + $0, thread_id: "r\($0)")])
        }
        reading[0].latestThreadId = "s20"
        reading[0].items[0].thread_id = "s20"
        reading[2].latestThreadId = "s3"
        reading[2].items[0].thread_id = "s3"
        store.sitrep.standing = standing
        store.zones.reading = reading
        let fold = BoardFold.forYourEyes

        let nowStanding = standing.prefix(fold).map(\.thread_id)
        let nowReading = reading.prefix(BoardFold.reading).map(\.latestThreadId).filter { $0 != "s3" }
        let laterStanding = standing.dropFirst(fold).map(\.thread_id).filter { $0 != "s20" }
        let laterReading = reading.dropFirst(BoardFold.reading).map(\.latestThreadId)
        expect(fold == 10 && BoardFold.reading == 24, "the suite runs the Mac's folds")
        let visible = nowStanding.count + nowReading.count
        let total = visible + laterStanding.count + laterReading.count

        // THE ROWS ON SCREEN AT ONCE: For-your-eyes, then the Reading cards,
        // the shared thread once, and the card whose row is below the fold now.
        await warmer.warmSitrepOnce(store: store)
        expect(Array(cache.asked.prefix(fold)).sorted() == nowStanding.sorted(),
               "the For-your-eyes rows on screen go first: \(cache.asked)")
        expect(Set(cache.asked.dropFirst(fold)) == Set(nowReading) && cache.asked.contains("s20"),
               "then the Reading cards on screen, one whose row is further down included: \(cache.asked)")
        expect(cache.asked.count == visible, "and nothing else yet: \(cache.asked)")
        expect(ImageStore.shared.reconciled == 1
            && Set(ImageStore.shared.pins) == Set(cache.asked.map(ThreadPrefetch.messageId)),
               "every warmed body's images are pinned")
        expect(cache.lifetimes == [ThreadPrefetch.warmFresh], "a warm keeps its threads for the warm lifetime")

        // THE REST WAITS: for the settle, out an open reader, and a settle
        // after it closes.
        store.threadId = "open"
        try? await Task.sleep(for: .milliseconds(600))
        expect(cache.asked.count == visible, "nothing more goes while a reader is open: \(cache.asked.count)")
        store.threadId = nil
        try? await Task.sleep(for: .milliseconds(150))
        expect(cache.asked.count == visible, "nor straight after it closes: \(cache.asked.count)")
        await warmer.deferred?.value
        expect(cache.asked.count == total + visible && Set(cache.asked).count == total,
               "then the rest go, each once: \(cache.asked.count) of \(total) + \(visible)")
        expect(Set(cache.asked.suffix(visible)) == Set(nowStanding + nowReading),
               "and last, the rows on screen again: \(cache.asked.suffix(visible))")
        expect(Set(cache.asked.dropFirst(visible).prefix(laterStanding.count)) == Set(laterStanding),
               "For-your-eyes before Reading in the deferred half too")
        expect(Set(HeroCache.shared.resolved) == Set(laterReading) && HeroCache.shared.resolved.count == laterReading.count,
               "the heroes behind the Reading expander are resolved: \(HeroCache.shared.resolved)")

        // THE BOARD AS IT IS WHEN THE REST GOES: a row resolved and a Reading
        // sender pruned during the wait are not warmed.
        cache.asked = []
        await warmer.warmSitrepOnce(store: store)
        store.sitrep.standing.removeLast()
        store.resolvedIds = [130]
        await warmer.deferred?.value
        expect(!cache.asked.contains("s29") && !cache.asked.contains("r30"),
               "resolved and pruned threads are left out: \(cache.asked)")
        expect(cache.asked.count == total - 2 + visible, "and nothing else is: \(cache.asked.count)")
        store.sitrep.standing = standing
        store.resolvedIds = []

        // ON SCREEN FIRST in the deferred half: a row that failed at launch
        // and is no longer withheld, one the board now names newer mail in,
        // one that moved on screen, then a Reading card that did, all before
        // anything below the folds. A thread ThreadPrefetch is withholding,
        // a failure still inside its minute included, is not asked for.
        cache.asked = []
        cache.failing = ["s1", "s4"]
        await warmer.warmSitrepOnce(store: store)
        cache.failing = []
        expect(cache.withheld == ["s1", "s4"], "a failure is withheld: \(cache.withheld)")
        cache.withheld = ["s4", "s15"]
        store.sitrep.standing[2].id = 99_999
        store.sitrep.standing.remove(at: 5)
        store.zones.reading.remove(at: 3)
        await warmer.deferred?.value
        let deferredHalf = Array(cache.asked.dropFirst(visible))
        expect(Set(deferredHalf.prefix(3)) == ["s1", "s2", "s10"],
               "the rows on screen not warmed yet go first: \(deferredHalf.prefix(4))")
        expect(deferredHalf.dropFirst(3).first == "r24",
               "then the card that moved on screen, before the rows below the fold: \(deferredHalf.prefix(5))")
        expect(!deferredHalf.contains("s4") && !deferredHalf.contains("s15"),
               "withheld threads are left alone: \(deferredHalf)")
        store.sitrep.standing = standing
        store.zones.reading = reading
        cache.withheld = []

        // A sender with one of two messages resolved keeps its card, pointed
        // at the other: the warm follows the card.
        cache.asked = []
        store.zones.reading[1].latestThreadId = "r1-new"
        store.zones.reading[1].items = [
            AttentionUpdate(id: 501, thread_id: "r1-new"), AttentionUpdate(id: 101, thread_id: "r1"),
        ]
        store.resolvedIds = [501]
        await warmer.warmSitrepOnce(store: store)
        expect(cache.asked.contains("r1") && !cache.asked.contains("r1-new"),
               "a re-pointed card is warmed at the thread it now shows: \(cache.asked)")
        let repointed = warmer.deferred
        warmer.resetForSwitch()
        await repointed?.value
        store.zones.reading = reading
        store.resolvedIds = []

        // THE DEFERRED HALF PAUSES for a reader opened while it runs, and
        // stops for remote images turned off or a costly path.
        cache.delay = .milliseconds(20)
        let everyThread = Set(standing.map(\.thread_id) + reading.map(\.latestThreadId))
        func midPass(_ flip: () -> Void, _ unflip: () -> Void, stops: Bool, _ what: String) async {
            cache.asked = []
            ImageStore.shared.pins = []
            await warmer.warmSitrepOnce(store: store)
            await eventually { cache.asked.count > visible + 4 }
            flip()
            try? await Task.sleep(for: .milliseconds(100))
            let held = cache.asked.count
            try? await Task.sleep(for: .milliseconds(300))
            expect(cache.asked.count == held && held < total, "\(what): it holds at \(held) of \(total)")
            unflip()
            await warmer.deferred?.value
            if stops {
                expect(cache.asked.count == held, "\(what): and does not come back: \(cache.asked.count)")
            } else {
                // Every thread on the board ends up pinned.
                expect(Set(ImageStore.shared.pins) == Set(everyThread.map(ThreadPrefetch.messageId)),
                       "\(what): and every thread is pinned once it closes")
            }
        }
        await midPass({ store.threadId = "open" }, { store.threadId = nil }, stops: false, "a reader opened")
        await midPass({ Prefs.shared.loadRemoteImages = false }, { Prefs.shared.loadRemoteImages = true },
                      stops: true, "remote images turned off")
        await midPass({ costly.on = true }, { costly.on = false }, stops: true, "a costly path")
        await midPass({ store.scheduledPreviewRow = 1 }, { store.scheduledPreviewRow = nil }, stops: false,
                      "a scheduled send's preview opened")

        // A constraint seen once ends the pass, though it lifts at once.
        cache.asked = []
        await warmer.warmSitrepOnce(store: store)
        await eventually { cache.asked.count > visible + 4 }
        let seen = Flag()
        warmer.constrained = { _ in
            if costly.on { seen.on = true }
            return costly.on
        }
        costly.on = true
        await eventually { seen.on }
        costly.on = false
        try? await Task.sleep(for: .milliseconds(50))
        let stoppedAt = cache.asked.count
        await warmer.deferred?.value
        expect(cache.asked.count == stoppedAt && stoppedAt < total,
               "a constraint that lifts does not bring the pass back: \(stoppedAt) then \(cache.asked.count)")
        warmer.constrained = { _ in costly.on }

        // Mail resolved after the deferred half has started is not pinned back.
        cache.asked = []
        await warmer.warmSitrepOnce(store: store)
        await eventually { cache.asked.count > visible + 4 }
        store.sitrep.standing.removeLast(5)
        await warmer.deferred?.value
        expect(!cache.asked.contains { ["s25", "s26", "s27", "s28", "s29"].contains($0) },
               "rows resolved mid-pass are dropped from the groups still to go: \(cache.asked.suffix(20))")
        store.sitrep.standing = standing
        cache.delay = .zero

        // Each message is checked as its images go: one resolved here is
        // skipped, and the rest of its thread still warms.
        cache.asked = []
        ImageStore.shared.pins = []
        cache.older = ["s0"]
        store.resolvedIds = [ThreadPrefetch.messageId("s0^")]
        await warmer.warmSitrepOnce(store: store)
        expect(!ImageStore.shared.pins.contains(ThreadPrefetch.messageId("s0^"))
            && ImageStore.shared.pins.contains(ThreadPrefetch.messageId("s0")),
               "a resolved message keeps no pin, the newer one in its thread does: \(ImageStore.shared.pins.count)")
        cache.older = []
        store.resolvedIds = []
        let resolved = warmer.deferred
        warmer.resetForSwitch()
        await resolved?.value

        // NOT AT ALL on a constrained device.
        cache.asked = []
        costly.on = true
        await warmer.warmSitrepOnce(store: store)
        await warmer.deferred?.value
        expect(cache.asked.count == visible, "a constrained device warms only what is on screen: \(cache.asked.count)")
        costly.on = false

        // NOR FOR A WORLD SWITCHED AWAY FROM.
        cache.asked = []
        await warmer.warmSitrepOnce(store: store)
        let abandoned = warmer.deferred
        warmer.resetForSwitch()
        await abandoned?.value
        expect(cache.asked.count == visible, "an account switch abandons the deferred half: \(cache.asked.count)")
        cache.asked = []
        await warmer.warmSitrepOnce(store: store)
        store.epoch += 1
        await warmer.deferred?.value
        expect(cache.asked.count == visible, "and so does a new store epoch: \(cache.asked.count)")

        // THE NEXT LANDING UNDER A NEW EPOCH warms again, once per epoch, a
        // switch's or not.
        cache.asked = []
        let idle = warmer.deferred
        warmer.noteSitrepLanded()
        warmer.noteZonesLanded()
        await eventually { warmer.deferred != idle }
        expect(cache.asked.count == visible, "both landed, the warm starts: \(cache.asked.count)")
        let first = warmer.deferred
        store.epoch += 1
        cache.asked = []
        warmer.noteSitrepLanded()
        try? await Task.sleep(for: .milliseconds(100))
        expect(cache.asked.isEmpty, "one half landed under a new epoch starts nothing: \(cache.asked.count)")
        warmer.noteZonesLanded()
        warmer.noteSitrepLanded()
        await eventually { warmer.deferred != first }
        let second = warmer.deferred
        warmer.resetForSwitch()
        await first?.value
        await second?.value
        expect(cache.asked.count == visible, "a new epoch's landing warms again, once: \(cache.asked.count)")

        // What the Reading zone draws is the pruned list.
        cache.asked = []
        store.resolvedIds = [101]
        await warmer.warmSitrepOnce(store: store)
        expect(!cache.asked.contains("r1") && cache.asked.contains("r24"),
               "a sender pruned off the board is not warmed, and the next card moves on screen: \(cache.asked)")
        store.resolvedIds = []
        let pruned = warmer.deferred
        warmer.resetForSwitch()
        await pruned?.value

        // The remote-images pref is absolute.
        cache.asked = []
        Prefs.shared.loadRemoteImages = false
        await warmer.warmSitrepOnce(store: store)
        try? await Task.sleep(for: .milliseconds(500))
        expect(cache.asked.isEmpty, "nothing is fetched with remote images off: \(cache.asked)")
        print("ImageWarmer tests passed")
    }
}
