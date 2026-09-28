// THE CLEAR BOARD's arithmetic and words: the streak of clear days, and what
// the clear band says about today. Foundation only, so it tests without a
// view in sight (see Views/ClearBand.swift for the view).
//
// CLEAR DAYS: the local days the sitrep was seen all clear, and how many of
// them run back unbroken from today. Kept on this device only, in defaults:
// it is a small reward for the person at this keyboard, not a fact about the
// mailbox, so it has no business on the wire.

import Foundation

enum ClearStreak {
    static let defaultsKey = "sitrep.clearDays"

    /// How many days are remembered. Enough for any run worth mentioning,
    /// small enough that nobody ever has to prune it.
    static let memory = 90

    /// `days` with `day` added: deduped, ascending, only the newest `memory`.
    /// Day keys are "YYYY-MM-DD", so string order IS date order.
    static func record(_ day: String, into days: [String]) -> [String] {
        let all = Set(days).union([day]).sorted()
        return Array(all.suffix(memory))
    }

    /// Consecutive clear days ending at `today`, counting today. Zero when
    /// today itself is not clear: a run that ended yesterday is not a run the
    /// board should brag about.
    static func run(endingAt today: Date, in days: [String], calendar: Calendar) -> Int {
        let set = Set(days)
        var count = 0
        var cursor = today
        while set.contains(DayKey.key(cursor, calendar: calendar)) {
            count += 1
            guard let prior = calendar.date(byAdding: .day, value: -1, to: cursor) else { break }
            cursor = prior
        }
        return count
    }

    /// Records today as clear and returns the run it extends.
    @discardableResult
    static func markToday(
        _ now: Date = Date(), calendar: Calendar = .current,
        defaults: UserDefaults = .standard
    ) -> Int {
        let today = DayKey.key(now, calendar: calendar)
        let days = record(today, into: defaults.stringArray(forKey: defaultsKey) ?? [])
        defaults.set(days, forKey: defaultsKey)
        return run(endingAt: now, in: days, calendar: calendar)
    }
}

enum ClearCopy {
    /// The window the daemon's "today" actually covers, in the reader's words.
    ///
    /// /client/mail-activity buckets by UTC day, so its latest row starts at
    /// UTC midnight: 5 PM the day before in California, 1 AM in Berlin.
    /// Calling that "today" was wrong for most of the planet, and in the US
    /// it reset every evening, which read as "No mail today" after a full
    /// day of it. So say where the window starts instead, and "today" only
    /// where UTC midnight IS local midnight.
    static func window(utcDay key: String, now: Date = Date(), calendar: Calendar = .current) -> String {
        var utc = Calendar(identifier: .gregorian)
        utc.timeZone = TimeZone(identifier: "UTC")!
        guard let start = DayKey.date(key, calendar: utc) else { return "today" }
        let localMidnight = calendar.startOfDay(for: now)
        if start == localMidnight { return "today" }
        let time = start.formatted(
            Date.FormatStyle(date: .omitted, time: .shortened, timeZone: calendar.timeZone))
        return start < localMidnight ? "since \(time) yesterday" : "since \(time)"
    }

    static func tally(received: Int, noise: Int, attention: Int, window: String = "today") -> String {
        guard received > 0 else {
            return window == "today"
                ? "No mail today. Suspiciously peaceful."
                : "No mail \(window). Suspiciously peaceful."
        }
        let came = "\(received) email\(received == 1 ? "" : "s") came in \(window)."
        let handled = attention == 1
            ? "the one that mattered is handled."
            : "the \(attention) that mattered are handled."
        switch (noise > 0, attention > 0) {
        case (true, true): return "\(came) \(noise) squelched as noise, and \(handled)"
        case (true, false): return "\(came) \(noise) squelched as noise. None needed you."
        case (false, true): return "\(came) " + handled.prefix(1).uppercased() + handled.dropFirst()
        case (false, false): return came
        }
    }
}
