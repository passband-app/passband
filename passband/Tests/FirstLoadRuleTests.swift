// The first-load veil's decision and progress (Lib/FirstLoadRule.swift),
// where every bug the veil has had lived: a walk whose counts have met is
// still running, absent is finished, disk stats never decide, and a lost
// connection lifts the veil without stamping.

import Foundation

@main
struct FirstLoadRuleTests {
    static func main() {
        typealias R = FirstLoadRule
        let running = R.Walk(done: 1_510, total: 4_200)
        let met = R.Walk(done: 4_200, total: 4_200)

        expect(R.decide(seen: true, connected: true, fresh: true, walk: running, hasMail: true, waited: false) == .never,
               "an existing user never sees it")
        expect(R.decide(seen: false, connected: true, fresh: true, walk: running, hasMail: true, waited: false) == .show,
               "a new user with a walk running sees it")
        expect(R.decide(seen: false, connected: true, fresh: true, walk: met, hasMail: true, waited: false) == .show,
               "counts that have met are still a walk (inbox done, Sent not yet listed)")
        expect(R.decide(seen: false, connected: true, fresh: true, walk: nil, hasMail: true, waited: false) == .conclude,
               "mail and no walk goes straight to the board")
        expect(R.decide(seen: false, connected: true, fresh: true, walk: nil, hasMail: false, waited: false) == .wait,
               "nothing yet waits")
        expect(R.decide(seen: false, connected: true, fresh: true, walk: nil, hasMail: false, waited: true) == .conclude,
               "and stops waiting")
        expect(R.decide(seen: false, connected: true, fresh: false, walk: running, hasMail: true, waited: false) == .wait,
               "stats from disk never decide")
        expect(R.decide(seen: false, connected: false, fresh: true, walk: running, hasMail: true, waited: false) == .wait,
               "no connection, no decision")

        expect(R.progress(connected: true, fresh: true, walk: running) == .running(finishingUp: false), "running")
        expect(R.progress(connected: true, fresh: true, walk: met) == .running(finishingUp: true),
               "met counts keep running, finishing up")
        expect(R.progress(connected: true, fresh: true, walk: nil) == .finished, "absent is finished")
        expect(R.progress(connected: true, fresh: false, walk: nil) == .hold, "stale stats change nothing")
        expect(R.progress(connected: false, fresh: true, walk: running) == .lost, "a lost connection lifts it")
        print("First load rule tests passed")
    }

    static func expect(_ ok: Bool, _ what: String) {
        guard ok else { fatalError(what) }
    }
}
