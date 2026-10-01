// THE ALL-MAIL LIST'S SHAPE, kept out of the view so it can be asserted: which
// rows a page shows (to do / everything, and the filter bar's lens), the day
// sections they fall into, and the time a row prints. Everything here is a pure
// function of the rows and a clock.
//
// The page exists for people who read mail as a list, and three things make a
// list readable to them that the old flat one lacked: finished mail that is
// still findable, a date heading over the rows, and a way to cut five hundred
// rows to the twelve they came for.

import Foundation

/// The filter bar's lenses over the inbox page. Each narrows the rows already
/// fetched; none changes what is fetched. Reminders is NOT one of these: it
/// swaps the data source (`AppStore.reminderFilter`), and the bar shows it as
/// a peer only so the two can never both be on.
enum MailFilter: String, CaseIterable, Sendable {
    case all, important, deadlines, attachments

    var label: String {
        switch self {
        case .all: "All"
        case .important: "Important"
        case .deadlines: "Deadlines"
        case .attachments: "Attachments"
        }
    }

    var help: String {
        switch self {
        case .all: "every message"
        case .important: "mail triage picked for your attention"
        case .deadlines: "mail with a date it needs you by"
        case .attachments: "mail with files attached"
        }
    }

    /// IMPORTANT IS THE SITREP'S OWN PICK: tier `signal` is exactly the set the
    /// model chose to show you, so this lens is the sitrep's judgement laid
    /// over the list instead of a second, client-side guess at it.
    func matches(_ u: AttentionUpdate) -> Bool {
        switch self {
        case .all: true
        case .important: u.tier != .noise
        case .deadlines: u.displayDeadline != nil
        case .attachments: u.hasAttachments
        }
    }
}

enum MailList {
    /// How many rows from the end of what is loaded the infinite scroll asks
    /// for the next page: far enough ahead that it lands before you get there.
    static let loadAhead = 40

    /// Whether a row reads as finished: done on the server, or resolved in
    /// this session ahead of the poll that will say so. The overlay is what
    /// lets `u` bring a row straight back instead of one poll later.
    static func isDone(_ u: AttentionUpdate, resolved: Set<Int>) -> Bool {
        u.status == .done || resolved.contains(u.id)
    }

    /// The rows a mail page shows, in the order fetched.
    static func visible(
        _ rows: [AttentionUpdate], showDone: Bool, filter: MailFilter, resolved: Set<Int>
    ) -> [AttentionUpdate] {
        rows.filter { u in
            (showDone || !isDone(u, resolved: resolved)) && filter.matches(u)
        }
    }

    /// One date heading and the rows under it. `start` is the first row's
    /// position in the flat list, which is what the keyboard cursor counts.
    struct Section: Equatable {
        var title: String
        var start: Int
        var rows: [AttentionUpdate]
        /// Stable across a row leaving above it, unlike `start`: the heading
        /// and its first mail, so SwiftUI keeps the section's identity.
        var id: String { "\(title)|\(rows.first?.id ?? -1)" }
    }

    /// Within each day, the mail that matters most first: importance, then
    /// arrival. Rows come in newest first, so days are contiguous runs; each
    /// run is re-sorted on its own and the days keep their order.
    static func ordered(
        _ rows: [AttentionUpdate], calendar: Calendar = .current
    ) -> [AttentionUpdate] {
        var out: [AttentionUpdate] = []
        out.reserveCapacity(rows.count)
        var run: [(Int, AttentionUpdate)] = []
        var runDay: Date?? = nil
        func flush() {
            run.sort { a, b in
                a.1.importance != b.1.importance ? a.1.importance > b.1.importance : a.0 < b.0
            }
            out.append(contentsOf: run.map(\.1))
            run.removeAll(keepingCapacity: true)
        }
        for (i, u) in rows.enumerated() {
            let day = Fmt.date(u.arrivedAt).map { calendar.startOfDay(for: $0) }
            if runDay != nil, runDay! != day { flush() }
            runDay = .some(day)
            run.append((i, u))
        }
        flush()
        return out
    }

    /// Counts per lens, for the filter bar's chips.
    static func counts(
        _ rows: [AttentionUpdate], showDone: Bool, resolved: Set<Int>
    ) -> [MailFilter: Int] {
        var out: [MailFilter: Int] = [:]
        for u in rows where showDone || !isDone(u, resolved: resolved) {
            for f in MailFilter.allCases where f.matches(u) { out[f, default: 0] += 1 }
        }
        return out
    }

    /// EVERYTHING THE PAGE DRAWS, computed once per change rather than once
    /// per read. The page's body runs on every cursor move and every hover,
    /// and reads its rows a dozen times each run; filtering, ordering and
    /// heading five hundred rows that often is what made the list stutter.
    /// The key compares cheaply: arrays and sets that share storage compare by
    /// identity first, and the cached page is the same storage until a poll
    /// actually changes it.
    @MainActor
    final class Listing {
        static let shared = Listing()
        struct Output {
            var rows: [AttentionUpdate] = []
            var sections: [Section] = []
        }
        private struct Key: Equatable {
            var source: [AttentionUpdate]
            var showDone: Bool
            var filter: MailFilter
            var resolved: Set<Int>
            var grouped: Bool
            var day: Date
        }
        private var key: Key?
        private var output = Output()

        /// `grouped` false keeps the source order and draws no headings (the
        /// reminder lens, sorted by when each comes back).
        func of(
            _ source: [AttentionUpdate], showDone: Bool, filter: MailFilter,
            resolved: Set<Int>, grouped: Bool, now: Date = Date()
        ) -> Output {
            let k = Key(
                source: source, showDone: showDone, filter: filter, resolved: resolved,
                grouped: grouped, day: Calendar.current.startOfDay(for: now))
            if k == key { return output }
            let shown = MailList.visible(
                source, showDone: showDone, filter: filter, resolved: resolved)
            if grouped {
                let rows = MailList.ordered(shown)
                output = Output(rows: rows, sections: MailList.sections(rows, now: now))
            } else {
                output = Output(rows: shown, sections: [])
            }
            key = k
            return output
        }
    }

    /// The same, for the filter bar's counts.
    @MainActor
    final class Counts {
        static let shared = Counts()
        private var key: ([AttentionUpdate], Bool, Set<Int>)?
        private var value: [MailFilter: Int] = [:]
        func of(_ rows: [AttentionUpdate], showDone: Bool, resolved: Set<Int>) -> [MailFilter: Int] {
            if let key, key.0 == rows, key.1 == showDone, key.2 == resolved { return value }
            value = MailList.counts(rows, showDone: showDone, resolved: resolved)
            key = (rows, showDone, resolved)
            return value
        }
    }

    /// Group rows (already newest first) under day headings: Today, Yesterday,
    /// the weekday for the rest of the past week, "Last week", then one per
    /// month. Contiguous runs only, so an out-of-order row opens a new section
    /// rather than being moved away from where the sort put it.
    static func sections(
        _ rows: [AttentionUpdate], now: Date = Date(), calendar: Calendar = .current
    ) -> [Section] {
        var out: [Section] = []
        // One title per calendar day, not per row: a 30-day page has at most
        // ~31 distinct days, and formatting is the expensive part.
        var titles: [Date: String] = [:]
        for (i, u) in rows.enumerated() {
            let date = Fmt.date(u.arrivedAt)
            let title: String
            if let date {
                let day = calendar.startOfDay(for: date)
                if let hit = titles[day] {
                    title = hit
                } else {
                    title = dayTitle(date, now: now, calendar: calendar)
                    titles[day] = title
                }
            } else {
                title = dayTitle(nil, now: now, calendar: calendar)
            }
            if out.last?.title == title {
                out[out.count - 1].rows.append(u)
            } else {
                out.append(Section(title: title, start: i, rows: [u]))
            }
        }
        return out
    }

    static func dayTitle(_ date: Date?, now: Date, calendar: Calendar = .current) -> String {
        // No date means the daemon has not said when it arrived, and the only
        // mail in that state is mail that has only just come in.
        guard let date else { return "Today" }
        let today = calendar.startOfDay(for: now)
        let day = calendar.startOfDay(for: date)
        let days = calendar.dateComponents([.day], from: day, to: today).day ?? 0
        // A clock a little behind the sender's still files their mail as today.
        if days <= 0 { return "Today" }
        if days == 1 { return "Yesterday" }
        func fmt(_ style: Date.FormatStyle) -> String {
            var style = style
            style.timeZone = calendar.timeZone
            return date.formatted(style)
        }
        if days < 7 { return fmt(.dateTime.weekday(.wide)) }
        if days < 14 { return "Last week" }
        let sameYear = calendar.isDate(date, equalTo: now, toGranularity: .year)
        if calendar.isDate(date, equalTo: now, toGranularity: .month) {
            return "Earlier in \(fmt(.dateTime.month(.wide)))"
        }
        return sameYear
            ? fmt(.dateTime.month(.wide))
            : fmt(.dateTime.month(.wide).year())
    }

    /// The row's stamp, the way a mail client prints one: a time today, a date
    /// before that, and the year only once it is not this one. Memoized per
    /// stamp string and day: a row's body runs on every hover.
    @MainActor static func stamp(_ iso: String?, now: Date = Date()) -> String {
        guard let iso else { return "" }
        let day = Calendar.current.startOfDay(for: now)
        if stampDay != day {
            stampCache = [:]
            stampDay = day
        }
        if let hit = stampCache[iso] { return hit }
        let value = stamp(Fmt.date(iso), now: now)
        stampCache[iso] = value
        return value
    }
    @MainActor private static var stampCache: [String: String] = [:]
    @MainActor private static var stampDay: Date?

    static func stamp(_ date: Date?, now: Date = Date(), calendar: Calendar = .current) -> String {
        guard let date else { return "" }
        func fmt(_ style: Date.FormatStyle) -> String {
            var style = style
            style.timeZone = calendar.timeZone
            return date.formatted(style)
        }
        if calendar.isDate(date, inSameDayAs: now) {
            return fmt(.dateTime.hour().minute())
        }
        if calendar.isDate(date, equalTo: now, toGranularity: .year) {
            return fmt(.dateTime.month(.abbreviated).day())
        }
        return fmt(.dateTime.month(.abbreviated).day().year())
    }
}
