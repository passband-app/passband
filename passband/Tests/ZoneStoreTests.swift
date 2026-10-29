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

    static func shipment(_ id: Int) -> Shipment {
        let json = """
            {"id": \(id), "account_id": 0, "tracking_number": "1Z\(id)", "carrier": "ups",
             "item_name": "parcel \(id)", "status": "in_transit", "thread_id": "s\(id)",
             "first_seen": "2026-10-01T00:00:00Z", "last_update": "2026-10-01T00:00:00Z"}
            """
        return try! JSONDecoder().decode(Shipment.self, from: Data(json.utf8))
    }

    static func bill(_ id: Int) -> BankingRecord {
        BankingRecord(
            id: id, message_id: id, thread_id: "b\(id)", from_addr: "bank@example.com", kind: .bill,
            institution: "Bank", amount: 40, currency: "USD", account_hint: nil,
            received_at: "2026-10-01T00:00:00Z")
    }

    /// A value with every zone filled, so a write that forgets one is seen.
    static func full(_ id: Int) -> SitrepZoneCache {
        SitrepZoneCache(
            calendar: [card(id)], shipments: [shipment(id)], banking: [bill(id)],
            receipts: [receipt(id)], reading: [sender(id)], readingLoaded: true)
    }

    static func sameZones(_ a: SitrepZoneCache, _ b: SitrepZoneCache) -> Bool {
        a.calendar == b.calendar && a.shipments == b.shipments && a.banking == b.banking
            && a.receipts == b.receipts && a.reading == b.reading && a.readingLoaded == b.readingLoaded
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
        let start = full(1)
        zones.replace(with: start)
        check(sameZones(zones.value, start), "a whole value lands in every zone")

        // These two hold with or without replace()'s guards: the Observation
        // macro itself skips an equal write. They pin the behavior the views
        // rely on; the guards are tested through the hooks below.
        check(
            !invalidates(
                { _ = (zones.calendar, zones.shipments, zones.banking, zones.receipts, zones.reading, zones.readingLoaded) },
                by: { zones.replace(with: start) }),
            "putting back the same value invalidates no reader (macro + guards)")
        var next = start
        next.receipts = [receipt(2)]
        check(
            !invalidates(
                { _ = (zones.calendar, zones.shipments, zones.banking, zones.reading, zones.readingLoaded) },
                by: { zones.replace(with: next) }),
            "a value that differs in receipts leaves the other zones' readers alone (macro + guards)")
        check(zones.receipts == [receipt(2)], "and receipts took the new rows")

        zones.replace(with: SitrepZoneCache())
        check(sameZones(zones.value, SitrepZoneCache()), "a reset empties every zone, shipments and banking included")
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
        zones.shipments = [shipment(1)]
        check(persisted == 2, "a shipments write schedules a save")
        zones.reading = [sender(1)]
        check(reading == 1 && persisted == 2, "a reading write moves the reading threads, and saves nothing")

        // The one check of replace()'s guards: a didSet runs on an equal
        // write even where Observation does not notify.
        zones.replace(with: zones.value)
        check(persisted == 2 && reading == 1, "putting back the same value fires no hook")
    }

    static func theValueRoundTrips() {
        let zones = SitrepZones()
        zones.replace(with: full(3))
        check(sameZones(zones.value, full(3)), "a value put in comes back out whole, all six fields")
    }
}
