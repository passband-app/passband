// The clear board's streak and words, pinned. A streak that counts a broken
// run, or a tally that says "the 1 that mattered are handled", is the reward
// turning into a small embarrassment, and nothing on screen would catch it
// short of clearing a real inbox on several consecutive days.

import Foundation

@main
@MainActor
struct ClearBoardTests {
    static var failures = 0
    static var checks = 0

    static let cal: Calendar = {
        var c = Calendar(identifier: .gregorian)
        c.timeZone = TimeZone(identifier: "America/Los_Angeles")!
        return c
    }()

    static func main() {
        recordDedupesSortsAndForgets()
        runCountsBackFromToday()
        markTodayPersistsAndExtends()
        tallyReadsRightForEveryShape()
        windowSaysWhereTheUTCDayStarts()

        if failures > 0 {
            print("FAILED: \(failures) of \(checks) checks")
            exit(1)
        }
        print("ok: \(checks) checks passed")
    }

    static func day(_ key: String, hour: Int = 12) -> Date {
        DayKey.date(key, calendar: cal)!.addingTimeInterval(Double(hour) * 3600)
    }

    static func recordDedupesSortsAndForgets() {
        let r = ClearStreak.record("2026-09-02", into: ["2026-09-03", "2026-09-01", "2026-09-02"])
        expect(r == ["2026-09-01", "2026-09-02", "2026-09-03"], "deduped and ascending: \(r)")
        let many = (1...120).map { String(format: "2026-%02d-%02d", 1 + $0 / 28, 1 + $0 % 28) }
        let kept = ClearStreak.record("2026-12-31", into: many)
        expect(kept.count == ClearStreak.memory, "keeps only the newest \(ClearStreak.memory)")
        expect(kept.last == "2026-12-31", "the newest day survives")
    }

    static func runCountsBackFromToday() {
        let days = ["2026-09-01", "2026-09-03", "2026-09-04", "2026-09-05"]
        expect(ClearStreak.run(endingAt: day("2026-09-05"), in: days, calendar: cal) == 3, "three back to back")
        expect(ClearStreak.run(endingAt: day("2026-09-06"), in: days, calendar: cal) == 0, "yesterday's run is not today's")
        expect(ClearStreak.run(endingAt: day("2026-09-01"), in: days, calendar: cal) == 1, "a lone day is one")
        expect(ClearStreak.run(endingAt: day("2026-09-05", hour: 23), in: days, calendar: cal) == 3, "late night is still today")
        let month = ["2026-08-30", "2026-08-31", "2026-09-01"]
        expect(ClearStreak.run(endingAt: day("2026-09-01"), in: month, calendar: cal) == 3, "across a month boundary")
    }

    static func markTodayPersistsAndExtends() {
        let suite = "clear-board-tests-\(UUID().uuidString)"
        let d = UserDefaults(suiteName: suite)!
        defer { d.removePersistentDomain(forName: suite) }
        expect(ClearStreak.markToday(day("2026-09-10"), calendar: cal, defaults: d) == 1, "first clear day")
        expect(ClearStreak.markToday(day("2026-09-10", hour: 18), calendar: cal, defaults: d) == 1, "twice in a day is one day")
        expect(ClearStreak.markToday(day("2026-09-11"), calendar: cal, defaults: d) == 2, "the next day extends")
        expect(ClearStreak.markToday(day("2026-09-13"), calendar: cal, defaults: d) == 1, "a skipped day resets")
        expect(d.stringArray(forKey: ClearStreak.defaultsKey)?.count == 3, "three distinct days stored")
    }

    static func tallyReadsRightForEveryShape() {
        func eq(_ got: String, _ want: String) { expect(got == want, "got \"\(got)\", want \"\(want)\"") }
        eq(ClearCopy.tally(received: 0, noise: 0, attention: 0), "No mail today. Suspiciously peaceful.")
        eq(ClearCopy.tally(received: 84, noise: 79, attention: 5),
           "84 emails came in today. 79 squelched as noise, and the 5 that mattered are handled.")
        eq(ClearCopy.tally(received: 12, noise: 11, attention: 1),
           "12 emails came in today. 11 squelched as noise, and the one that mattered is handled.")
        eq(ClearCopy.tally(received: 9, noise: 9, attention: 0),
           "9 emails came in today. 9 squelched as noise. None needed you.")
        eq(ClearCopy.tally(received: 1, noise: 0, attention: 1),
           "1 email came in today. The one that mattered is handled.")
        eq(ClearCopy.tally(received: 3, noise: 0, attention: 0), "3 emails came in today.")
        for n in [0, 1, 5] {
            for a in [0, 1, 5] {
                let s = ClearCopy.tally(received: n + a + 1, noise: n, attention: a)
                expect(!s.contains("\u{2014}"), "no em dashes in copy: \(s)")
            }
        }
    }

    static func windowSaysWhereTheUTCDayStarts() {
        func zone(_ id: String) -> Calendar {
            var c = Calendar(identifier: .gregorian)
            c.timeZone = TimeZone(identifier: id)!
            return c
        }
        func at(_ iso: String) -> Date { ISO8601DateFormatter().date(from: iso)! }
        // 3 PM in LA on the 10th is 22:00 UTC: still UTC day the 10th, which
        // began at 5 PM LA time on the 9th.
        let la = ClearCopy.window(utcDay: "2026-09-10", now: at("2026-09-10T22:00:00Z"), calendar: zone("America/Los_Angeles"))
        expect(la.hasPrefix("since 5") && la.hasSuffix("yesterday"), "LA afternoon: \(la)")
        // 7 PM in LA on the 10th is the 11th in UTC, which began at 5 PM today.
        let eve = ClearCopy.window(utcDay: "2026-09-11", now: at("2026-09-11T02:00:00Z"), calendar: zone("America/Los_Angeles"))
        expect(eve.hasPrefix("since 5") && !eve.hasSuffix("yesterday"), "LA evening: \(eve)")
        // Berlin in summer: the UTC day starts at 2 AM local, today.
        let ber = ClearCopy.window(utcDay: "2026-09-10", now: at("2026-09-10T10:00:00Z"), calendar: zone("Europe/Berlin"))
        expect(ber.hasPrefix("since 2") && !ber.hasSuffix("yesterday"), "Berlin: \(ber)")
        // UTC itself is the one place the daemon's day is the local day.
        let utc = ClearCopy.window(utcDay: "2026-09-10", now: at("2026-09-10T10:00:00Z"), calendar: zone("UTC"))
        expect(utc == "today", "UTC: \(utc)")
        expect(ClearCopy.tally(received: 3, noise: 3, attention: 0, window: "since 5:00 PM yesterday")
            == "3 emails came in since 5:00 PM yesterday. 3 squelched as noise. None needed you.", "window in copy")
        expect(ClearCopy.tally(received: 0, noise: 0, attention: 0, window: "since 1:00 AM")
            == "No mail since 1:00 AM. Suspiciously peaceful.", "empty window in copy")
    }

    static func expect(_ cond: Bool, _ what: String) {
        checks += 1
        if !cond {
            failures += 1
            print("  FAIL: \(what)")
        }
    }
}
