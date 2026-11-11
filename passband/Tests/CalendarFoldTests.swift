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
        _ id: Int, _ title: String?, _ start: String?, received: String, change: String? = nil,
        thread: String? = nil, message: Int? = nil
    ) -> CalendarUpdate {
        CalendarUpdate(
            id: id, message_id: message ?? id, thread_id: thread ?? "t\(id)", kind: .event,
            event_title: title,
            starts_at: start, organizer: nil, received_at: received, change: change)
    }

    static let party = "Northside Collective Presents Halcyon Drift"

    static func main() {
        theTicketMailAndItsReminderAreOneRow()
        aTimedAndADateOnlyStartOnOneDayAreOne()
        anotherDayOrAnotherTitleStaysApart()
        aDatelessRowNeverBridgesTwoDays()
        orderAndUntitledRows()
        newsKeepsItsOwnRow()
        looseTitlesDoNotFold()
        anOffsetDoesNotMakeANewStart()
        aRelayedThreadIsOneEvent()
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
        let short = row(4, "halcyon  DRIFT", "2026-11-06", received: "2026-11-05T00:00:00Z")
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

    static func newsKeepsItsOwnRow() {
        let order = row(1, party, "2026-11-06T19:00:00-08:00", received: "2026-11-04T01:00:00Z")
        // "Doors moved to 9pm": its own row, with the new time.
        let moved = row(
            2, party, "2026-11-06T21:00:00-08:00", received: "2026-11-05T01:00:00Z",
            change: "updated")
        check(CalendarUpdate.folded([order, moved]).map(\.id) == [1, 2], "a move keeps its row")
        let off = row(3, party, "2026-11-06", received: "2026-11-05T02:00:00Z", change: "cancelled")
        check(CalendarUpdate.folded([order, off]).map(\.id) == [1, 3], "a cancellation keeps its row")
        // Even without the change mark, two different times are two rows.
        let later = row(4, party, "2026-11-06T21:00:00-08:00", received: "2026-11-05T03:00:00Z")
        check(CalendarUpdate.folded([order, later]).count == 2, "two times, two rows")
        // A plain reminder still folds.
        let reminder = row(5, party, nil, received: "2026-11-05T04:00:00Z", change: "created")
        check(CalendarUpdate.folded([order, reminder]).map(\.id) == [1], "a reminder folds")
    }

    static func looseTitlesDoNotFold() {
        let gym = row(1, "Gym", "2026-11-06", received: "2026-11-04T01:00:00Z")
        let meet = row(2, "Gymnastics meet", "2026-11-06", received: "2026-11-04T02:00:00Z")
        check(CalendarUpdate.folded([gym, meet]).count == 2, "not inside a word")
        let art = row(3, "art", "2026-11-06", received: "2026-11-04T03:00:00Z")
        let party = row(4, "party at mel's", "2026-11-06", received: "2026-11-04T04:00:00Z")
        check(CalendarUpdate.folded([art, party]).count == 2, "not a short title")
        // A short title cannot chain two lunches through itself.
        let sam = row(5, "Lunch with Sam", "2026-11-06", received: "2026-11-04T05:00:00Z")
        let lunch = row(6, "Lunch", "2026-11-06", received: "2026-11-04T06:00:00Z")
        let ana = row(7, "Lunch with Ana", "2026-11-06", received: "2026-11-04T07:00:00Z")
        check(CalendarUpdate.folded([sam, lunch, ana]).count == 3, "no chain through Lunch")
    }

    static func anOffsetDoesNotMakeANewStart() {
        // One mail's 3:30pm carries an offset, the next one's does not.
        let a = row(1, party, "2026-11-13T15:30:00-08:00", received: "2026-11-11T13:00:00Z")
        let b = row(2, party, "2026-11-13T15:30:00", received: "2026-11-11T06:00:00Z")
        check(CalendarUpdate.folded([a, b]).map(\.id) == [2], "one start, offset or not")
        let c = row(3, party, "2026-11-13T16:30:00", received: "2026-11-11T07:00:00Z")
        check(CalendarUpdate.folded([a, c]).count == 2, "another wall clock stays apart")
    }

    static func aRelayedThreadIsOneEvent() {
        // A rental site relays each landlord message into one thread, and the
        // model titles the same tour a little differently every time.
        let t = "relay"
        let first = row(
            1, "Apartment tour at 25 Main St", "2026-11-13T15:30:00", received: "2026-11-11T05:00:00Z",
            change: "agreed", thread: t)
        let second = row(
            2, "Rental tour: 25 Main St", "2026-11-13T15:30:00", received: "2026-11-11T06:00:00Z",
            change: "agreed", thread: t)
        let third = row(
            3, "Rental tour: 25 Main St", "2026-11-13T15:30:00-08:00",
            received: "2026-11-11T13:00:00Z", change: "agreed", thread: t)
        let meetup = row(4, "Agent meetup", "2026-11-17T17:30:00-08:00", received: "2026-11-11T02:00:00Z")
        check(
            CalendarUpdate.folded([third, second, first, meetup]).map(\.id) == [1, 4],
            "the thread is one row, the earliest mail's")
        // A dateless message in the thread joins by title only: it may be
        // another event of the trip.
        let chatter = row(5, "Rental tour: 25 Main St", nil, received: "2026-11-11T14:00:00Z", thread: t)
        check(CalendarUpdate.folded([chatter, second]).map(\.id) == [2], "dateless restatement folds")
        let car = row(11, "Car rental pickup", nil, received: "2026-11-11T14:00:00Z", thread: t)
        check(CalendarUpdate.folded([car, first]).count == 2, "dateless other event stands")
        // A date-only row is not a start the thread rule trusts: it would
        // match every time of its day.
        let flight = row(12, "Flight to Lisbon", "2026-11-13T07:00:00-08:00", received: "2026-11-11T01:00:00Z", thread: "trip")
        let hotel = row(13, "Hotel check-in", "2026-11-13", received: "2026-11-11T02:00:00Z", thread: "trip")
        let cruise = row(14, "Dinner cruise", "2026-11-13T19:00:00", received: "2026-11-11T03:00:00Z", thread: "trip")
        check(CalendarUpdate.folded([flight, hotel, cruise]).count == 3, "a trip's day stands, oldest first")
        check(CalendarUpdate.folded([cruise, hotel, flight]).count == 3, "a trip's day stands, newest first")
        // Another time in the thread is another event (a second viewing).
        let later = row(
            6, "Second viewing", "2026-11-13T17:00:00", received: "2026-11-11T15:00:00Z", thread: t)
        check(CalendarUpdate.folded([first, later]).count == 2, "another time stays apart")
        // A move in the thread keeps its own row.
        let moved = row(
            7, "Rental tour: 25 Main St", "2026-11-13T15:30:00", received: "2026-11-11T16:00:00Z",
            change: "updated", thread: t)
        check(CalendarUpdate.folded([first, moved]).count == 2, "news keeps its row")
        // Two records the model split from ONE message never fold by thread.
        let x = row(8, "Matinee", "2026-11-13T15:30:00", received: "2026-11-11T05:00:00Z", thread: "o", message: 9)
        let y = row(9, "Workshop", "2026-11-13T15:30:00", received: "2026-11-11T05:00:00Z", thread: "o", message: 9)
        check(CalendarUpdate.folded([x, y]).count == 2, "one message's records stand")
        // Not even through a later mail restating one of them, in either order.
        let z = row(15, "Matinee reminder", "2026-11-13T15:30:00", received: "2026-11-12T05:00:00Z", thread: "o", message: 3)
        check(CalendarUpdate.folded([z, x, y]).count == 2, "split records stand, newest first")
        check(CalendarUpdate.folded([x, y, z]).count == 2, "split records stand, oldest first")
        // Same start in two threads, titles unrelated: two events.
        let other = row(10, "Dentist", "2026-11-13T15:30:00", received: "2026-11-11T05:00:00Z")
        check(CalendarUpdate.folded([first, other]).count == 2, "another thread stays apart")
    }
}
