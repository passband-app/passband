// The sitrep Calendar card's rows: one per event, not one per email
// (`CalendarUpdate.folded`, Model/WireTypes.swift). The case it was made for:
// RA's ticket order and RA's "download the app to view your tickets"
// reminder, two rows for one party.

import Foundation

@main
struct CalendarFoldTests {
    nonisolated(unsafe) static var failures = 0
    nonisolated(unsafe) static var checks = 0

    static func check(_ ok: Bool, _ what: String) {
        checks += 1
        if !ok {
            failures += 1
            print("FAIL: \(what)")
        }
    }

    static func row(
        _ id: Int, _ title: String?, _ start: String?, received: String
    ) -> CalendarUpdate {
        CalendarUpdate(
            id: id, message_id: id, thread_id: "t\(id)", kind: .event, event_title: title,
            starts_at: start, organizer: nil, received_at: received)
    }

    static let party = "Parameter & Endzeit Invite Polar Inertia"

    static func main() {
        theTicketMailAndItsReminderAreOneRow()
        aTimedAndADateOnlyStartOnOneDayAreOne()
        anotherDayOrAnotherTitleStaysApart()
        aDatelessRowNeverBridgesTwoDays()
        orderAndUntitledRows()
        print("calendar-fold: \(checks - failures)/\(checks) passed")
        exit(failures == 0 ? 0 : 1)
    }

    static func theTicketMailAndItsReminderAreOneRow() {
        // The reminder arrives later and, as on screen, without a start.
        let order = row(1, party, "2026-11-06", received: "2026-11-04T01:11:34Z")
        let reminder = row(2, party, nil, received: "2026-11-04T19:00:37Z")
        let gym = row(3, "Gym", nil, received: "2026-11-04T12:00:00Z")
        let out = CalendarUpdate.folded([reminder, gym, order])
        check(out.map(\.id) == [1, 3], "one party row (the order's), and the gym: \(out.map(\.id))")
        // With a start on both, the earlier mail (the confirmation) shows.
        let dated = row(2, party, "2026-11-06", received: "2026-11-04T19:00:37Z")
        check(CalendarUpdate.folded([dated, order]).map(\.id) == [1], "the confirmation shows")
        // Case, spacing, and one title inside the other.
        let short = row(4, "polar  INERTIA", "2026-11-06", received: "2026-11-05T00:00:00Z")
        check(CalendarUpdate.folded([order, short]).map(\.id) == [1], "a title inside the other")
    }

    static func aTimedAndADateOnlyStartOnOneDayAreOne() {
        // The day is the start's own: 7pm Pacific is the 6th, though the 7th in UTC.
        let timed = row(1, party, "2026-11-06T19:00:00-08:00", received: "2026-11-04T01:00:00Z")
        let day = row(2, party, "2026-11-06", received: "2026-11-04T02:00:00Z")
        check(CalendarUpdate.folded([day, timed]).map(\.id) == [1], "timed and date-only fold")
    }

    static func anotherDayOrAnotherTitleStaysApart() {
        let fri = row(1, party, "2026-11-06", received: "2026-11-04T01:00:00Z")
        let sat = row(2, party, "2026-11-07", received: "2026-11-04T02:00:00Z")
        check(CalendarUpdate.folded([fri, sat]).count == 2, "two nights are two rows")
        let other = row(3, "Dentist", "2026-11-06", received: "2026-11-04T03:00:00Z")
        check(CalendarUpdate.folded([fri, other]).count == 2, "two events one day")
    }

    static func aDatelessRowNeverBridgesTwoDays() {
        let a = row(1, "Standup", "2026-11-06", received: "2026-11-04T01:00:00Z")
        let b = row(2, "Standup", "2026-11-13", received: "2026-11-04T02:00:00Z")
        let c = row(3, "Standup", nil, received: "2026-11-04T03:00:00Z")
        check(CalendarUpdate.folded([a, b, c]).map(\.id) == [1, 2, 3], "ambiguous: all stand")
        // Two dateless rows of one title are one.
        let d = row(4, "Standup", nil, received: "2026-11-04T04:00:00Z")
        check(CalendarUpdate.folded([c, d]).map(\.id) == [3], "two dateless rows fold")
    }

    static func orderAndUntitledRows() {
        // Untitled rows never fold, even with each other.
        let x = row(1, nil, "2026-11-06", received: "2026-11-04T01:00:00Z")
        let y = row(2, "  ", "2026-11-06", received: "2026-11-04T02:00:00Z")
        check(CalendarUpdate.folded([x, y]).count == 2, "untitled rows stand")
        // Each event keeps its first row's place.
        let p1 = row(3, party, nil, received: "2026-11-04T19:00:00Z")
        let g = row(4, "Gym", nil, received: "2026-11-04T12:00:00Z")
        let p2 = row(5, party, "2026-11-06", received: "2026-11-04T01:00:00Z")
        check(CalendarUpdate.folded([p1, g, p2]).map(\.id) == [5, 4], "the party keeps first place")
    }
}
