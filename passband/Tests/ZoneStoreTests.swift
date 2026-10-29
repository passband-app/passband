// The sitrep's zones as the store holds them (Model/ZoneStore.swift). What
// has to hold: a write to one zone invalidates the readers of that zone and
// no other, a whole-value write (a repaint, a reset) touches only the zones
// that differ, and the store's hooks fire only for the zones they are about.

import Foundation
import Observation

@main
@MainActor
struct ZoneStoreTests {
    static var failures = 0
    static var checks = 0

    static func main() {
        aZoneWriteInvalidatesOnlyItsOwnReaders()
        aWholeValueWritesOnlyTheZonesThatDiffer()
        theHooksFireOnlyForTheirZones()
        theValueRoundTrips()

        if failures > 0 {
            print("FAILED: \(failures) of \(checks) checks")
            exit(1)
        }
        print("ok: \(checks) checks passed")
    }

    static func check(_ ok: Bool, _ what: String) {
        checks += 1
        if !ok {
            failures += 1
            print("FAIL: \(what)")
        }
    }

    // MARK: - fixtures

    static func card(_ id: Int) -> CalendarUpdate {
        CalendarUpdate(
            id: id, message_id: id, thread_id: "t\(id)", kind: .invite, event_title: "event \(id)",
            starts_at: nil, organizer: nil, received_at: "2026-10-01T00:00:00Z")
    }

    static func receipt(_ id: Int) -> Receipt {
        Receipt(
            id: id, account_id: 0, message_id: id, thread_id: "t\(id)", from_addr: "shop@example.com",
            from_name: "Shop", amount: 12.5, currency: "USD", received_at: "2026-10-01T00:00:00Z")
    }

    static func sender(_ id: Int) -> ReadingSender {
        ReadingSender(
            address: "news\(id)@example.com", sender: "News \(id)", count: 1, summary: "s",
            latest: 0, latestThreadId: "r\(id)", items: [], rule: nil)
    }

    /// Whether reading `read` is invalidated by running `write`.
    static func invalidates(_ read: () -> Void, by write: () -> Void) -> Bool {
        // onChange is synchronous with the write; the box is only what lets
        // the Sendable closure report it.
        final class Flag: @unchecked Sendable { var fired = false }
        let flag = Flag()
        withObservationTracking(read, onChange: { flag.fired = true })
        write()
        return flag.fired
    }

    // MARK: - checks

    static func aZoneWriteInvalidatesOnlyItsOwnReaders() {
        let zones = SitrepZones()
        check(
            !invalidates({ _ = zones.calendar }, by: { zones.receipts = [receipt(1)] }),
            "a receipts write leaves a calendar reader alone")
        check(
            !invalidates({ _ = zones.receipts }, by: { zones.reading = [sender(1)] }),
            "a reading write leaves a receipts reader alone")
        check(
            !invalidates({ _ = zones.reading }, by: { zones.calendar = [card(1)] }),
            "a calendar write leaves a reading reader alone")
        check(
            invalidates({ _ = zones.calendar }, by: { zones.calendar = [card(2)] }),
            "a calendar write reaches a calendar reader")
        check(
            invalidates({ _ = zones.readingLoaded }, by: { zones.readingLoaded = true }),
            "readingLoaded is observed on its own")
    }

    static func aWholeValueWritesOnlyTheZonesThatDiffer() {
        let zones = SitrepZones()
        var start = SitrepZoneCache()
        start.calendar = [card(1)]
        start.receipts = [receipt(1)]
        start.reading = [sender(1)]
        start.readingLoaded = true
        zones.replace(with: start)

        check(
            !invalidates(
                { _ = (zones.calendar, zones.shipments, zones.banking, zones.receipts, zones.reading, zones.readingLoaded) },
                by: { zones.replace(with: start) }),
            "putting back the same value invalidates nothing")

        var next = start
        next.receipts = [receipt(2)]
        check(
            !invalidates({ _ = (zones.calendar, zones.reading, zones.readingLoaded) }, by: { zones.replace(with: next) }),
            "a value that differs in receipts leaves the other zones' readers alone")
        check(zones.receipts == [receipt(2)], "and receipts took the new rows")

        zones.replace(with: SitrepZoneCache())
        check(zones.calendar.isEmpty && zones.receipts.isEmpty && zones.reading.isEmpty, "a reset empties every zone")
        check(!zones.readingLoaded, "a reset forgets that Reading answered")
    }

    static func theHooksFireOnlyForTheirZones() {
        let zones = SitrepZones()
        var persisted = 0
        var reading = 0
        zones.onPersistedChange = { persisted += 1 }
        zones.onReadingChange = { reading += 1 }

        zones.receipts = [receipt(1)]
        zones.banking = []
        zones.readingLoaded = true
        check(persisted == 0 && reading == 0, "zones the board file does not keep schedule no save")

        zones.calendar = [card(1)]
        check(persisted == 1, "a calendar write schedules a save")
        zones.reading = [sender(1)]
        check(reading == 1 && persisted == 1, "a reading write moves the reading threads, and saves nothing")

        zones.replace(with: zones.value)
        check(persisted == 1 && reading == 1, "putting back the same value fires no hook")
    }

    static func theValueRoundTrips() {
        let zones = SitrepZones()
        var value = SitrepZoneCache()
        value.calendar = [card(3)]
        value.receipts = [receipt(3)]
        value.reading = [sender(3)]
        value.readingLoaded = true
        zones.replace(with: value)
        let back = zones.value
        check(
            back.calendar == value.calendar && back.receipts == value.receipts
                && back.reading == value.reading && back.readingLoaded && back.shipments.isEmpty
                && back.banking.isEmpty,
            "a value put in comes back out whole")
    }
}
