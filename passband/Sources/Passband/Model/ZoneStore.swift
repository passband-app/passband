import Foundation
import Observation

/// Everything the sitrep's zones render, held here rather than in the zones: a
/// view's `@State` dies on unmount, so the last good answer is already on screen
/// when the dashboard re-mounts and the refresh happens UNDERNEATH it — rows are
/// only ever replaced by newer rows, never cleared first. `AppStore.zonesLoadedAt`
/// keeps that honest in the other direction (see `refreshZones`).
struct SitrepZoneCache: Sendable {
    var calendar: [CalendarUpdate] = []
    var shipments: [Shipment] = []
    var banking: [BankingRecord] = []
    var receipts: [Receipt] = []
    var reading: [ReadingSender] = []
    /// Whether the reading fetch has ANSWERED for this account. An empty
    /// `reading` means "nothing to read" only once it has; before that it
    /// means nobody has asked yet, and the quiet board must not take the one
    /// for the other.
    var readingLoaded = false
}

/// The zones as the store holds them: ONE OBSERVED PROPERTY PER ZONE, so a
/// view that reads `store.zones.calendar` re-renders when the calendar moves
/// and not when a shipment does. As a single struct property, every zone write
/// invalidated every zone card and the board.
///
/// `replace(with:)` writes only the zones that differ. The Observation macro
/// already skips an equal write to an Equatable property, but a `didSet`
/// still runs on one, and the hooks below must not. `SitrepZoneCache` stays
/// the value that is copied around: a switch's snapshot, the board from disk.
@MainActor
@Observable
final class SitrepZones {
    var calendar: [CalendarUpdate] = [] {
        didSet { onPersistedChange?() }
    }
    var shipments: [Shipment] = [] {
        didSet { onPersistedChange?() }
    }
    var banking: [BankingRecord] = []
    var receipts: [Receipt] = []
    var reading: [ReadingSender] = [] {
        didSet { onReadingChange?() }
    }
    /// See `SitrepZoneCache.readingLoaded`.
    var readingLoaded = false

    /// A zone the board file keeps changed (`PersistedBoard` holds the
    /// calendar and the shipments, nothing else).
    @ObservationIgnored var onPersistedChange: (() -> Void)?
    /// The Reading zone changed, which moves `AppStore.readingThreadIds`.
    @ObservationIgnored var onReadingChange: (() -> Void)?

    /// The zones as one value, for a snapshot. It reads every zone, so a view
    /// body must never call it.
    var value: SitrepZoneCache {
        SitrepZoneCache(
            calendar: calendar, shipments: shipments, banking: banking,
            receipts: receipts, reading: reading, readingLoaded: readingLoaded)
    }

    /// Put a whole value back (a repaint, a reset), writing only the zones
    /// that differ.
    func replace(with next: SitrepZoneCache) {
        if calendar != next.calendar { calendar = next.calendar }
        if shipments != next.shipments { shipments = next.shipments }
        if banking != next.banking { banking = next.banking }
        if receipts != next.receipts { receipts = next.receipts }
        if reading != next.reading { reading = next.reading }
        if readingLoaded != next.readingLoaded { readingLoaded = next.readingLoaded }
    }
}

/// The records feed's three zones, derived where the feed is fetched (off
/// the main actor) so the store only compares and assigns.
struct RecordsFeedZones: Sendable {
    var receipts: [Receipt]
    var banking: [BankingRecord]
    var calendar: [CalendarUpdate]

    init(_ feed: AgentFeed) {
        receipts = feed.receipts
        banking = feed.banking
        calendar = feed.calendar
    }
}
