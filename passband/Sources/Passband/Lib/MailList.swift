// THE ALL-MAIL LIST'S SHAPE, kept out of the view so it can be asserted: which
// rows a page shows (the filter bar's lens and the category picker), the day
// sections they fall into and their order inside each, how a poll's head merges
// into what the scroll loaded, and the time a row prints. Everything here is a
// pure function of the rows and a clock.
//
// The page exists for people who read mail as a list, and three things make a
// list readable to them that the old flat one lacked: finished mail that is
// still findable, a date heading over the rows, and a way to cut hundreds of
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
        case .all: "Every message"
        case .important: "Mail triage picked for your attention"
        case .deadlines: "Mail with a date it needs you by"
        case .attachments: "Mail with files attached"
        }
    }

    /// IMPORTANT IS THE SITREP'S OWN PICK: the For your eyes zone, so this lens
    /// is the sitrep's judgement laid over the list instead of a second,
    /// client-side guess at it.
    func matches(_ u: AttentionUpdate) -> Bool {
        switch self {
        case .all: true
        // The sitrep's own placement where the daemon sends one (a pin or a
        // fired reminder counts), the tier where it does not.
        case .important: u.placement.map { $0 == "eyes" } ?? (u.tier != .noise)
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
    ///
    /// `hiddenZones` are the category picker's unticked sitrep zones (the
    /// wire's `placement` strings). Mail with no placement (untriaged, or from
    /// an older daemon) has no category to hide by, so it always shows.
    static func visible(
        _ rows: [AttentionUpdate], showDone: Bool, filter: MailFilter, resolved: Set<Int>,
        hiddenZones: Set<String> = []
    ) -> [AttentionUpdate] {
        rows.filter { u in
            (showDone || !isDone(u, resolved: resolved)) && filter.matches(u)
                && !(u.placement.map(hiddenZones.contains) ?? false)
        }
    }

    /// One date heading and the rows under it. `start` is the first row's
    /// position in the flat list, which is what the keyboard cursor counts.
    struct Section: Equatable {
        var title: String
        var start: Int
        var rows: [AttentionUpdate]
        /// The heading, plus which time it appears (a stray undated row can
        /// open a second "Today" run). Stable across rows leaving or being
        /// re-ranked inside it, so SwiftUI keeps the section's identity.
        var occurrence = 0
        var id: String { "\(title)#\(occurrence)" }
    }

    /// Epoch for "order received": the arrival stamp, or on an older daemon
    /// `surfaced_at`. Undated rows (nothing has surfaced them yet: the newest
    /// mail) and unreadable ones sort to the top together.
    static func arrivalTS(_ u: AttentionUpdate) -> Double {
        guard let s = u.arrivedAt, let d = Fmt.date(s) else { return .greatestFiniteMagnitude }
        return d.timeIntervalSince1970
    }

    /// Newest first by arrival, ties on id. The key is parsed ONCE per row
    /// rather than twice per comparison.
    static func sortedByArrival(_ rows: [AttentionUpdate]) -> [AttentionUpdate] {
        rows.map { ($0, arrivalTS($0)) }
            .sorted { a, b in a.1 != b.1 ? a.1 > b.1 : a.0.id > b.0.id }
            .map(\.0)
    }

    /// THE POLL'S HEAD, MERGED INTO WHAT THE SCROLL HAS LOADED. Rows the head
    /// returns replace their old copies; loaded rows older than the head's
    /// last row stay; a loaded row inside the head's span that the head no
    /// longer returns has left the listing (spam, deleted) and goes.
    ///
    /// `reset` is true when the loaded rows cannot be trusted to continue the
    /// head: a FULL head that shares no row with them means more mail arrived
    /// than one head holds, and whatever came between the two is in neither.
    /// The caller then starts over from the head and lets the scroll refill.
    struct HeadMerge: Equatable {
        var rows: [AttentionUpdate]
        var reset: Bool
    }

    static func mergeHead(
        loaded: [AttentionUpdate]?, head: [AttentionUpdate], headIsFull: Bool
    ) -> HeadMerge {
        guard let loaded, !loaded.isEmpty, let floor = head.last else {
            return HeadMerge(rows: head, reset: true)
        }
        let headIds = Set(head.map(\.id))
        if headIsFull, !loaded.contains(where: { headIds.contains($0.id) }) {
            return HeadMerge(rows: head, reset: true)
        }
        let floorTS = arrivalTS(floor)
        let tail = loaded.filter { u in
            guard !headIds.contains(u.id) else { return false }
            let ts = arrivalTS(u)
            return ts < floorTS || (ts == floorTS && u.id < floor.id)
        }
        return HeadMerge(rows: head + tail, reset: false)
    }

    /// Day sections with the mail that matters most first IN EACH SECTION:
    /// importance, then arrival. Per section rather than per calendar day, so
    /// a heading that spans days ("Last week") is one ranking, not a sawtooth
    /// that restarts every midnight. `rows` come in newest first.
    static func rankedSections(
        _ rows: [AttentionUpdate], now: Date = Date(), calendar: Calendar = .current
    ) -> [Section] {
        var out = sections(rows, now: now, calendar: calendar)
        var start = 0
        for s in out.indices {
            out[s].rows = out[s].rows.enumerated()
                .sorted { a, b in
                    a.element.importance != b.element.importance
                        ? a.element.importance > b.element.importance : a.offset < b.offset
                }
                .map(\.element)
            out[s].start = start
            start += out[s].rows.count
        }
        return out
    }

    /// Counts per lens, for the filter bar's chips.
    static func counts(
        _ rows: [AttentionUpdate], showDone: Bool, resolved: Set<Int>,
        hiddenZones: Set<String> = []
    ) -> [MailFilter: Int] {
        var out: [MailFilter: Int] = [:]
        for u in rows
        where (showDone || !isDone(u, resolved: resolved))
            && !(u.placement.map(hiddenZones.contains) ?? false)
        {
            for f in MailFilter.allCases where f.matches(u) { out[f, default: 0] += 1 }
        }
        return out
    }

    /// EVERYTHING THE PAGE DRAWS, computed once per change rather than once
    /// per read. The page's body runs on every cursor move and every hover,
    /// and reads its rows a dozen times each run; filtering, ordering and
    /// heading every loaded row that often is what made the list stutter.
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
            var hiddenZones: Set<String>
            var grouped: Bool
            var day: Date
        }
        private var key: Key?
        private var output = Output()

        /// `grouped` false keeps the source order and draws no headings (the
        /// reminder lens, sorted by when each comes back).
        func of(
            _ source: [AttentionUpdate], showDone: Bool, filter: MailFilter,
            resolved: Set<Int>, hiddenZones: Set<String> = [], grouped: Bool,
            now: Date = Date()
        ) -> Output {
            let k = Key(
                source: source, showDone: showDone, filter: filter, resolved: resolved,
                hiddenZones: hiddenZones, grouped: grouped,
                day: Calendar.current.startOfDay(for: now))
            if k == key { return output }
            let shown = MailList.visible(
                source, showDone: showDone, filter: filter, resolved: resolved,
                hiddenZones: hiddenZones)
            if grouped {
                let sections = MailList.rankedSections(shown, now: now)
                output = Output(rows: sections.flatMap(\.rows), sections: sections)
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
        private var key: ([AttentionUpdate], Bool, Set<Int>, Set<String>)?
        private var value: [MailFilter: Int] = [:]
        func of(
            _ rows: [AttentionUpdate], showDone: Bool, resolved: Set<Int>,
            hiddenZones: Set<String> = []
        ) -> [MailFilter: Int] {
            if let key, key.0 == rows, key.1 == showDone, key.2 == resolved,
                key.3 == hiddenZones
            {
                return value
            }
            value = MailList.counts(
                rows, showDone: showDone, resolved: resolved, hiddenZones: hiddenZones)
            key = (rows, showDone, resolved, hiddenZones)
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
                let seen = out.filter { $0.title == title }.count
                out.append(Section(title: title, start: i, rows: [u], occurrence: seen))
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
