// The discovery ledger's rules: found on the first move, kept on the third
// distinct day, nothing ever regresses, and a blob from some other build never
// costs anybody their ring. The shell (UserDefaults, analytics, the practice
// flag) is out of reach of a headless suite, which is why every input here is
// an argument.
//
// The ring is only ever wrong LATER, and quietly: a set that keeps on the day
// it was found, or a downgrade that wipes the ledger, files no bug. So the
// boundaries are pinned here.

import Foundation

@main
@MainActor
struct DiscoveryLedgerTests {
    static var failures = 0
    static var checks = 0

    static func main() {
        firstNoteFinds()
        threeDaysKeepsOnce()
        daysUnionWithinASet()
        everyMoveHasASet()
        practiceOnlyCountsTheTour()
        oneCelebrationADay()
        backfillIsSilent()
        acknowledgeClearsFresh()
        roundTripAndTolerance()
        allKeptNeedsEverySet()
        theDebugOverride()

        if failures > 0 {
            print("FAILED: \(failures) of \(checks) checks")
            exit(1)
        }
        print("ok: \(checks) checks passed")
    }

    static let now = Date(timeIntervalSince1970: 1_790_000_000)

    static func firstNoteFinds() {
        var l = DiscoveryLedger()
        expect(l.state(of: .later) == .unseen, "a fresh ledger has seen nothing")
        expect(l.note(.reminderSet, day: "d1", now: now) == .found(.later), "first move finds")
        expect(l.state(of: .later) == .found, "and the state says so")
        expect(l.fresh == [.later], "a find is fresh")
        expect(l.note(.reminderSet, day: "d1", now: now) == nil, "same day again changes nothing")
        expect(l.state(of: .later) == .found, "still found, not kept")
        expect(l.sets[.later]?.days == ["d1"], "a same-day repeat adds no day")
        expect(l.moveCounts[.reminderSet] == 2, "but the move count still counts it")
    }

    static func threeDaysKeepsOnce() {
        var l = DiscoveryLedger()
        // Many moves on the finding day: none of them may keep.
        expect(l.note(.emailDone, day: "d1", now: now) == .found(.clear), "found on d1")
        for _ in 0..<10 { _ = l.note(.emailDone, day: "d1", now: now) }
        expect(l.state(of: .clear) == .found, "ten moves on one day are one day")
        expect(l.note(.emailDone, day: "d2", now: now) == nil, "the second day is not yet kept")
        expect(l.note(.emailDone, day: "d3", now: now) == .kept(.clear), "the third day keeps")
        expect(l.state(of: .clear) == .kept, "and the state says kept")
        expect(l.note(.emailDone, day: "d4", now: now) == nil, "kept is announced once")
        expect(l.note(.emailDone, day: "d3", now: now) == nil, "and not again on its own day")
        expect(l.state(of: .clear) == .kept, "nothing regresses")
        expect(
            (l.sets[.clear]?.days.count ?? 0) <= DiscoveryLedger.keptAfterDays,
            "the day list is capped")
    }

    static func daysUnionWithinASet() {
        var l = DiscoveryLedger()
        _ = l.note(.ruleSaved, day: "d1", now: now)
        _ = l.note(.triageCorrected, day: "d2", now: now)
        // Another set's moves on a third day count toward that set only.
        _ = l.note(.searchHitOpened, day: "d3", now: now)
        expect(l.state(of: .teach) == .found, "another set's day does not count toward teach")
        expect(
            l.note(.unsubscribed, day: "d3", now: now) == .kept(.teach),
            "three teach moves on three days keep teach, whichever moves they were")
        expect(l.state(of: .find) == .found, "find was found and nothing more")
    }

    static func everyMoveHasASet() {
        let covered = Set(Move.allCases.map(\.set))
        for set in FeatureSet.allCases {
            expect(covered.contains(set), "\(set) has at least one move")
        }
        expect(Move.tourCompleted.set == .clear, "the tour is clear")
        expect(Move.codeCopied.set == .glance, "a copied code is glance")
        expect(Move.groupSent.set == .write, "a group send is write")
        expect(Move.walkFinished.set == .flow, "a finished walk is flow")
    }

    static func practiceOnlyCountsTheTour() {
        for move in Move.allCases {
            expect(DiscoveryLedger.counts(move, practice: false), "\(move) counts on real mail")
            expect(
                DiscoveryLedger.counts(move, practice: true) == (move == .tourCompleted),
                "\(move) in practice counts only if it is the tour")
        }
    }

    static func oneCelebrationADay() {
        expect(DiscoveryLedger.mayCelebrate(lastCelebratedDay: nil, today: "d1"), "never celebrated")
        expect(!DiscoveryLedger.mayCelebrate(lastCelebratedDay: "d1", today: "d1"), "not twice a day")
        expect(DiscoveryLedger.mayCelebrate(lastCelebratedDay: "d1", today: "d2"), "a new day may")

        // The day key is the local calendar's, so it is pinned with one.
        var utc = Calendar(identifier: .gregorian)
        utc.timeZone = TimeZone(identifier: "UTC")!
        var tokyo = utc
        tokyo.timeZone = TimeZone(identifier: "Asia/Tokyo")!
        // 2026-10-01 20:00 UTC is 2026-10-02 05:00 in Tokyo.
        let evening = Date(timeIntervalSince1970: 1_790_884_800)
        expect(DiscoveryLedger.dayKey(evening, calendar: utc) == "2026-10-1", "utc day key")
        expect(DiscoveryLedger.dayKey(evening, calendar: tokyo) == "2026-10-2", "local day key")
    }

    static func backfillIsSilent() {
        var l = DiscoveryLedger()
        l.backfill(.tourCompleted, day: "d1", now: now)
        expect(l.state(of: .clear) == .found, "backfill finds clear")
        expect(l.fresh.isEmpty, "but nothing glows: an old user found nothing just now")
        // A set that was ALREADY fresh stays fresh through a backfill.
        var m = DiscoveryLedger()
        _ = m.note(.emailDone, day: "d1", now: now)
        m.backfill(.tourCompleted, day: "d1", now: now)
        expect(m.fresh == [.clear], "a backfill does not swallow a real find's glow")
    }

    static func acknowledgeClearsFresh() {
        var l = DiscoveryLedger()
        _ = l.note(.emailDone, day: "d1", now: now)
        _ = l.note(.reminderSet, day: "d1", now: now)
        expect(l.fresh == [.clear, .later], "two finds, two glows")
        l.acknowledge()
        expect(l.fresh.isEmpty, "the menu opening clears every glow")
        expect(l.state(of: .clear) == .found, "and leaves the finds themselves alone")
    }

    static func roundTripAndTolerance() {
        var l = DiscoveryLedger()
        _ = l.note(.emailDone, day: "d1", now: now)
        _ = l.note(.emailDone, day: "d2", now: now)
        _ = l.note(.emailDone, day: "d3", now: now)
        _ = l.note(.codeCopied, day: "d3", now: now)
        l.lastCelebratedDay = "d3"
        let back = DiscoveryLedger.decoded(from: l.encoded())
        expect(back == l, "encode/decode round trips")
        expect(back.state(of: .clear) == .kept, "kept survives the trip")

        // A blob from a later build: a set and a move this build never heard of.
        let future = """
            {"version": 7,
             "sets": {"clear": {"foundAt": 1, "days": ["d1"]},
                      "orbit": {"foundAt": 1, "keptAt": 2, "days": ["a","b","c"]}},
             "moves": {"emailDone": 4, "warpJump": 9},
             "fresh": ["clear", "orbit"],
             "lastCelebratedDay": "d9"}
            """
        let f = DiscoveryLedger.decoded(from: Data(future.utf8))
        expect(f.state(of: .clear) == .found, "a known set survives an unknown neighbour")
        expect(f.sets.count == 1, "the unknown set is dropped, not invented")
        expect(f.moveCounts == [.emailDone: 4], "the known move survives an unknown one")
        expect(f.fresh == [.clear], "unknown fresh entries are dropped")
        expect(f.lastCelebratedDay == "d9", "the celebration stamp survives")

        expect(DiscoveryLedger.decoded(from: Data("not json".utf8)) == DiscoveryLedger(), "garbage is empty")
        expect(DiscoveryLedger.decoded(from: nil) == DiscoveryLedger(), "nothing stored is empty")
        expect(DiscoveryLedger.decoded(from: Data("[1,2]".utf8)) == DiscoveryLedger(), "wrong shape is empty")
    }

    static func allKeptNeedsEverySet() {
        var l = DiscoveryLedger()
        let oneMovePerSet = FeatureSet.allCases.map { set in Move.allCases.first { $0.set == set }! }
        for day in ["d1", "d2", "d3"] {
            for move in oneMovePerSet.dropLast() { _ = l.note(move, day: day, now: now) }
        }
        expect(!l.allKept, "seven of eight kept is not all")
        for day in ["d1", "d2", "d3"] { _ = l.note(oneMovePerSet.last!, day: day, now: now) }
        expect(l.allKept, "eight of eight is")
    }

    static func theDebugOverride() {
        let states = DiscoveryLedger.parseOverride("clear:kept, flow:found,later:bogus,nope:kept")
        expect(states == [.clear: .kept, .flow: .found], "the override parses and skips junk")
        let l = DiscoveryLedger.forcing(states, now: now)
        expect(l.state(of: .clear) == .kept, "forced kept")
        expect(l.state(of: .flow) == .found, "forced found")
        expect(l.state(of: .later) == .unseen, "unlisted is unseen")
        expect(l.fresh.isEmpty, "nothing glows unless asked")

        let spec = "flow:fresh,later:kept,find:bogus"
        let glowing = DiscoveryLedger.forcing(
            DiscoveryLedger.parseOverride(spec),
            fresh: DiscoveryLedger.parseFreshOverride(spec), now: now)
        expect(glowing.state(of: .flow) == .found, "fresh is found")
        expect(glowing.fresh == [.flow], "fresh glows, kept does not")
        let unseenFresh = DiscoveryLedger.forcing([:], fresh: [.read], now: now)
        expect(unseenFresh.fresh.isEmpty, "an unseen set never glows")
    }

    static func expect(_ condition: Bool, _ message: String) {
        checks += 1
        if !condition {
            failures += 1
            print("FAIL: \(message)")
        }
    }
}
