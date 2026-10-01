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
    }

    /// Group rows (already newest first) under day headings: Today, Yesterday,
    /// the weekday for the rest of the past week, "Last week", then one per
    /// month. Contiguous runs only, so an out-of-order row opens a new section
    /// rather than being moved away from where the sort put it.
    static func sections(
        _ rows: [AttentionUpdate], now: Date = Date(), calendar: Calendar = .current
    ) -> [Section] {
        var out: [Section] = []
        for (i, u) in rows.enumerated() {
            let title = dayTitle(Fmt.date(u.arrivedAt), now: now, calendar: calendar)
            if out.last?.title == title {
                out[out.count - 1].rows.append(u)
            } else {
                out.append(Section(title: title, start: i, rows: [u]))
            }
        }
        return out
    }

    static func dayTitle(_ date: Date?, now: Date, calendar: Calendar = .current) -> String {
        guard let date else { return "Recent" }
        let today = calendar.startOfDay(for: now)
        let day = calendar.startOfDay(for: date)
        let days = calendar.dateComponents([.day], from: day, to: today).day ?? 0
        // A clock a little behind the sender's still files their mail as today.
        if days <= 0 { return "Today" }
        if days == 1 { return "Yesterday" }
        if days < 7 { return date.formatted(.dateTime.weekday(.wide)) }
        if days < 14 { return "Last week" }
        let sameYear = calendar.isDate(date, equalTo: now, toGranularity: .year)
        if calendar.isDate(date, equalTo: now, toGranularity: .month) {
            return "Earlier in \(date.formatted(.dateTime.month(.wide)))"
        }
        return sameYear
            ? date.formatted(.dateTime.month(.wide))
            : date.formatted(.dateTime.month(.wide).year())
    }

    /// The row's stamp, the way a mail client prints one: a time today, a date
    /// before that, and the year only once it is not this one.
    static func stamp(_ date: Date?, now: Date = Date(), calendar: Calendar = .current) -> String {
        guard let date else { return "" }
        if calendar.isDate(date, inSameDayAs: now) {
            return date.formatted(.dateTime.hour().minute())
        }
        if calendar.isDate(date, equalTo: now, toGranularity: .year) {
            return date.formatted(.dateTime.month(.abbreviated).day())
        }
        return date.formatted(.dateTime.month(.abbreviated).day().year())
    }
}
