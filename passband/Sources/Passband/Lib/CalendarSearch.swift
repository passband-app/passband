// THE SEARCH PANEL'S EVENTS BLOCK (docs/CALENDAR.md §10.1), the pure half:
// which query goes to which search, how the daemon's hits become rows, how
// many of them the block shows, and what each row says. The panel
// (Views/SidePanel.swift SearchView) draws it; nothing here touches the store
// or the network, so test.sh holds every rule.
//
// Events are a block of their own rather than rows in the mail ranking: an
// email's date is when it arrived and an event's is when it happens, and the
// two scores are not comparable. The daemon has already put the hits in
// order (upcoming soonest first, then past, most recent first); this keeps it.

import Foundation

enum CalendarSearch {
    /// The scope operator: events only, and every one of them.
    static let scopeToken = "in:calendar"

    /// Rows the block shows before "N more".
    static let blockCap = 3

    /// What the panel asks the daemon for at most. The block is three rows,
    /// and "N more" opens the rest of what came back.
    static let fetchLimit = 20

    /// Events only (`in:calendar`): the daemon's most.
    static let listLimit = 50

    /// The field, split by scope. `eventsOnly` when it holds `in:calendar`
    /// (any case); `text` is the field without it, words in order, single
    /// spaces.
    static func scope(_ query: String) -> (eventsOnly: Bool, text: String) {
        let tokens = query.split(whereSeparator: { $0.isWhitespace || $0.isNewline })
        let kept = tokens.filter { $0.lowercased() != scopeToken }
        return (kept.count != tokens.count, kept.joined(separator: " "))
    }

    /// Whether the mail search sits this query out: `in:calendar` asks for
    /// events alone, so the mail search (and with it the deeper-search lane)
    /// never runs and the operator never reaches `/client/search`.
    static func eventsOnly(_ query: String) -> Bool { scope(query).eventsOnly }

    /// What `/client/calendar/search` is asked, or nil when there is nothing
    /// to ask: no word of two characters or more, a `from:` still waiting
    /// for its value, or an operator only mail has (`from:`, `to:`,
    /// `in:sent`), which the daemon would answer with no events anyway.
    static func eventQuery(_ query: String) -> String? {
        let text = scope(query).text
        let tokens = text.split(separator: " ")
        var words = 0
        for token in tokens {
            let lower = token.lowercased()
            if mailOnly.contains(where: { lower.hasPrefix($0) }) { return nil }
            if lower.hasPrefix("after:") || lower.hasPrefix("before:") { continue }
            if token.filter({ $0.isLetter || $0.isNumber }).count >= 2 { words += 1 }
        }
        return words > 0 ? text : nil
    }

    private static let mailOnly = ["from:", "to:", "in:sent"]

    /// The daemon's hits as agenda rows, in the daemon's order. A row the
    /// agenda would not draw (a declined event, a start that does not parse)
    /// is not drawn here either.
    static func items(_ page: CalendarSearchPage, calendar: Calendar) -> [AgendaItem] {
        page.items.compactMap { hit in
            if let event = hit.event { return Agenda.item(event, calendar: calendar) }
            if let ghost = hit.ghost { return Agenda.item(ghost, calendar: calendar) }
            return nil
        }
    }

    /// What the block draws: the rows on show, and how many more the daemon
    /// has (`total` counts past its own limit, so "N more" is the truth even
    /// when the rest were not fetched). Expanded shows every fetched row.
    struct Block: Equatable, Sendable {
        let shown: [AgendaItem]
        let more: Int
    }

    static func block(_ items: [AgendaItem], total: Int, expanded: Bool) -> Block {
        let shown = expanded ? items : Array(items.prefix(blockCap))
        // Never fewer than the fetched rows left out: a daemon whose total
        // undercounts must not hide rows behind "0 more".
        let more = max(max(total, items.count) - shown.count, 0)
        return Block(shown: shown, more: expanded ? 0 : more)
    }

    /// "1 more event", "4 more events".
    static func moreLabel(_ n: Int) -> String {
        n == 1 ? "1 more event" : "\(n) more events"
    }

    /// Over: it ended (or, with no end, its hour or day ran out) before now.
    static func isPast(_ item: AgendaItem, now: Date) -> Bool {
        item.effectiveEnd <= now
    }

    /// The row's second line: "Thu Nov 6 · 7:30pm · Nopa", "Sat Nov 15 · all
    /// day", a year added when it is not this one ("Sep 12, 2025 · 9am").
    /// Times are the user's zone, which is `calendar`'s.
    static func label(_ item: AgendaItem, now: Date, calendar: Calendar) -> String {
        let sameYear = calendar.component(.year, from: item.start) == calendar.component(.year, from: now)
        let day = Agenda.format(item.start, sameYear ? "EEE MMM d" : "MMM d, yyyy", calendar)
        let time = item.allDay ? "all day" : Agenda.clock(item.start, calendar)
        return ([day, time] + [item.location].compactMap { $0 }).joined(separator: " · ")
    }

    /// The local midnight of the day the row opens on in the Calendar page.
    static func day(_ item: AgendaItem, calendar: Calendar) -> Date {
        calendar.startOfDay(for: item.start)
    }
}
