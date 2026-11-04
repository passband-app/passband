// The first-load veil's "about 4 minutes left" (Lib/CatchUpEstimate.swift).
// The line is only worth showing if it is honest, so the refusals are pinned
// as hard as the arithmetic: no estimate from one poll, from two a moment
// apart, or from a count standing still.

import Foundation

@main
struct CatchUpEstimateTests {
    static let t0 = Date(timeIntervalSince1970: 1_790_000_000)

    static func main() {
        refusesUntilItHasSeenMovement()
        rateOverTheWindow()
        oneSlowPageDoesNotSwingIt()
        aRestartedWalkStartsOver()
        repeatedPollsAreOneSample()
        phrases()
        theSharedLine()
        print("Catch-up estimate tests passed")
    }

    static func refusesUntilItHasSeenMovement() {
        var e = CatchUpEstimate()
        expect(e.rate == nil, "no samples, no rate")
        e.record(done: 100, at: t0)
        expect(e.remaining(total: 1000) == nil, "one sample is not a rate")
        e.record(done: 140, at: t0.addingTimeInterval(2))
        expect(e.rate == nil, "two samples two seconds apart are too close to believe")
        var still = CatchUpEstimate()
        still.record(done: 100, at: t0)
        still.record(done: 100, at: t0.addingTimeInterval(30))
        expect(still.rate == nil, "a count standing still says nothing about how long is left")
        expect(CatchUpEstimate.phrase(still.remaining(total: 1000)) == "estimating…", "and says so")
    }

    static func rateOverTheWindow() {
        var e = CatchUpEstimate()
        for i in 0..<4 { e.record(done: 100 + i * 100, at: t0.addingTimeInterval(Double(i) * 10)) }
        expect(e.rate == 10, "300 emails over 30 seconds is 10 a second, got \(String(describing: e.rate))")
        expect(e.remaining(total: 1600) == 120, "1,200 left at 10 a second is two minutes")
        expect(e.remaining(total: 100) == 0, "past the total is nothing left, never negative")
    }

    static func oneSlowPageDoesNotSwingIt() {
        var e = CatchUpEstimate()
        for i in 0..<7 { e.record(done: i * 100, at: t0.addingTimeInterval(Double(i) * 10)) }
        // A page that took a minute instead of ten seconds.
        e.record(done: 700, at: t0.addingTimeInterval(120))
        let rate = e.rate ?? 0
        expect(rate > 5 && rate < 10, "the window keeps the slow page from halving the rate twice over, got \(rate)")
        expect(e.samples.count == CatchUpEstimate.window, "the window holds its size")
        e.record(done: 800, at: t0.addingTimeInterval(130))
        expect(e.samples.count == CatchUpEstimate.window, "and drops the oldest to keep it")
        expect(e.samples.first?.done == 100, "the oldest went first")
    }

    static func aRestartedWalkStartsOver() {
        var e = CatchUpEstimate()
        e.record(done: 900, at: t0)
        e.record(done: 1000, at: t0.addingTimeInterval(10))
        e.record(done: 20, at: t0.addingTimeInterval(20))
        expect(e.samples.count == 1, "a count that went backwards is a new walk")
        expect(e.rate == nil, "with nothing yet to say about it")
    }

    static func repeatedPollsAreOneSample() {
        var e = CatchUpEstimate()
        e.record(done: 10, at: t0)
        e.record(done: 10, at: t0)
        expect(e.samples.count == 1, "the same instant twice is one sample")
    }

    static func phrases() {
        let cases: [(TimeInterval?, String)] = [
            (nil, "estimating…"),
            (12, "less than a minute left"),
            (70, "about a minute left"),
            (4 * 60 + 10, "about 4 minutes left"),
            (59 * 60, "about 59 minutes left"),
            (62 * 60, "about an hour left"),
            (95 * 60, "about 1.5 hours left"),
            (3 * 3600 + 60, "about 3 hours left"),
        ]
        for (seconds, want) in cases {
            let got = CatchUpEstimate.phrase(seconds)
            expect(got == want, "\(String(describing: seconds))s reads \(want), got \(got)")
            expect(!got.contains("\u{2014}") && !got.contains("\u{2013}"), "no dashes in copy")
        }
    }

    /// The veil and the board's banner print the same line from one estimate.
    static func theSharedLine() {
        var e = CatchUpEstimate()
        expect(e.line(done: 1510, total: 4200).hasSuffix("emails · estimating…"), "no pace yet: \(e.line(done: 1510, total: 4200))")
        for i in 0..<4 { e.record(done: 1000 + i * 120, at: t0.addingTimeInterval(Double(i) * 10)) }
        let line = e.line(done: 1360, total: 4200)
        expect(line.hasSuffix("emails · about 4 minutes left"), "12 a second over 2,840 left: \(line)")
        // Counts that have met with the walk still reported: finishing up,
        // never a pace and never "ready".
        let met = e.line(done: 4200, total: 4200)
        expect(met.hasSuffix("emails · finishing up…"), "met counts: \(met)")
    }

    static func expect(_ condition: Bool, _ message: String) {
        guard condition else { fatalError(message) }
    }
}
