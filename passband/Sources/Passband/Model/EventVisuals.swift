// THE PICTURE ON AN EVENT'S CARD, fetched. Which one is chosen, and why, is
// Lib/EventVisualPick.swift; this file only goes and gets it.
//
//   1. A photo from the mail behind the event: the newest message from
//      someone else in each of its threads, newest thread first, up to
//      `EventVisualPick.photoCandidates` of its images. Only while the user
//      lets mail load remote images (Settings), fetched the way every hero
//      is (`HeroBytes`: no cookies, no referrer, no cache), and only a
//      photo-shaped image that is not a logo on a flat banner.
//   2. The venue, from Apple MapKit: a search for the card's place, kept only
//      when Apple's answer is the place named; its Look Around scene, or a
//      map with a pin where Apple has none. What goes to Apple is the place
//      text alone, as in the location field (docs/SECURITY.md, the calendar
//      section). No key, no bill, no location permission.
//   3. Nothing.
//
// One answer per event per session, "nothing" included, so reopening a card
// does not search again. Wiped with the account.
//
// ON DISK TOO (`EventVisualDisk`, below), so a card paints its picture, or
// knows it has none, on its FIRST frame: before, every card opened at its
// short height and grew 136pt half a second later, when the resolve landed.
// The disk answer is read in `body`; the session's resolve still runs once
// and replaces it, except that a resolve finding nothing (offline, a slow
// MapKit) never erases a picture the disk holds.

import CryptoKit
import Foundation
import MapKit

/// What an event card shows above its title.
enum EventVisual {
    case photo(PlatformImage, threadId: String)
    /// The scene is nil while a card painted from disk waits for Apple's;
    /// `still` is the frame last seen there, drawn in the meantime.
    case lookAround(MKLookAroundScene?, MKMapItem, still: PlatformImage?)
    case map(MKMapItem)

    var mapItem: MKMapItem? {
        switch self {
        case .photo: nil
        case .lookAround(_, let item, _), .map(let item): item
        }
    }
}

@MainActor
final class EventVisuals {
    static let shared = EventVisuals()

    /// The rendered photo's long side, in pixels: the card at 2x.
    private nonisolated static let photoPixel = 1000
    private static let limit = 128

    /// key -> what it resolved to. A nil value is a settled "nothing".
    private var memo: [String: EventVisual?] = [:]
    private var order: [String] = []
    /// Bumped by `wipe`, so a resolve from the old account files nothing.
    private var generation = 0

    private init() {}

    /// What this session resolved, without starting any work: `.some(nil)`
    /// is "nothing to show", `nil` is "not looked yet".
    func cached(_ key: String) -> EventVisual?? { memo[key] }

    /// What a card should draw on its first frame: this session's answer,
    /// else the one on disk. Cheap enough for `body` (the disk read is once
    /// per key per session).
    func known(_ key: String) -> EventVisual?? {
        if let hit = memo[key] { return hit }
        return disk.visual(key)
    }

    private let disk = EventVisualDisk.shared

    func wipe() {
        memo.removeAll()
        order.removeAll()
        disk.wipeDecoded()
        generation += 1
    }

    /// The card's picture: a photo from `threadIds` (oldest first, as a
    /// ghost lists them), else the venue at `place`, else nil.
    func resolve(key: String, threadIds: [String], place: String?) async -> EventVisual? {
        if let known = memo[key] { return known }
        let started = generation
        var found: EventVisual?
        if Prefs.shared.loadRemoteImages {
            var seen = Set<String>()
            for threadId in threadIds.reversed() where seen.insert(threadId).inserted {
                if let image = await photo(threadId: threadId) {
                    found = .photo(image, threadId: threadId)
                    break
                }
            }
        }
        if found == nil, let place, EventVisualPick.wantsVenue(place) {
            found = await venue(place)
        }
        guard !Task.isCancelled, generation == started else { return found }
        // Nothing found is not proof there is nothing: MapKit offline, a
        // thread fetch timing out. The picture the disk holds stays.
        if found == nil, let kept = disk.visual(key) ?? nil {
            remember(key, kept)
            return kept
        }
        remember(key, found)
        disk.store(key, found)
        if case .lookAround(let scene?, _, still: nil) = found {
            Task { await self.keepStill(key: key, scene: scene, started: started) }
        }
        return found
    }

    /// One frame of the Look Around scene, for the disk: what the card draws
    /// next launch while Apple's scene loads. Taken after the card has its
    /// answer, so the first open waits on nothing new.
    private func keepStill(key: String, scene: MKLookAroundScene, started: Int) async {
        let options = MKLookAroundSnapshotter.Options()
        options.size = CGSize(width: 600, height: EventVisualView.height * 2)
        guard let shot = try? await MKLookAroundSnapshotter(scene: scene, options: options).snapshot,
            generation == started, let png = EventVisualDisk.png(shot.image)
        else { return }
        disk.storeStill(key, png: png)
    }

    private func remember(_ key: String, _ visual: EventVisual?) {
        if memo.updateValue(visual, forKey: key) == nil { order.append(key) }
        while order.count > Self.limit { memo.removeValue(forKey: order.removeFirst()) }
    }

    // MARK: - from the mail

    private func photo(threadId: String) async -> PlatformImage? {
        guard let view = try? await ThreadPrefetch.shared.fetch(threadId, fresh: 600),
            let newest = view.messages.last(where: { !$0.fromMe }),
            let html = newest.html, !html.isEmpty
        else { return nil }
        for src in Trackers.extractImageSrcs(html, limit: EventVisualPick.photoCandidates) {
            guard !Task.isCancelled else { return nil }
            guard let url = URL(string: src), let bytes = await HeroBytes.shared.load(url) else {
                continue
            }
            let png = await Task.detached(priority: .utility) { Self.render(bytes) }.value
            if let png, let image = PlatformImage(data: png) { return image }
        }
        return nil
    }

    /// The photo at card size, or nil for anything that is not one: too
    /// small, the wrong shape, or a logo on a flat field.
    private nonisolated static func render(_ bytes: Data) -> Data? {
        guard let image = Raster.thumbnail(bytes, maxPixel: photoPixel),
            EventVisualPick.isPhoto(width: image.width, height: image.height),
            !ImageTrim.hasFlatField(image)
        else { return nil }
        return Raster.png(image)
    }

    // MARK: - from Apple Maps

    private func venue(_ place: String) async -> EventVisual? {
        let request = MKLocalSearch.Request()
        request.naturalLanguageQuery = place
        request.resultTypes = [.pointOfInterest, .address]
        guard let response = try? await MKLocalSearch(request: request).start(),
            let item = response.mapItems.first(where: {
                EventVisualPick.venueMatches(
                    place: place, name: $0.name, address: $0.address?.fullAddress)
            })
        else { return nil }
        if let scene = try? await MKLookAroundSceneRequest(mapItem: item).scene {
            return .lookAround(scene, item, still: nil)
        }
        return .map(item)
    }
}

/// EVENT PICTURES ON DISK: which picture each card had, so the next launch
/// paints it on the first frame. One index (kind, the thread a photo came
/// from, the venue's point and name) plus a PNG per photo or Look Around
/// still, in Application Support's Passband/EventVisualCache.
///
/// Mail content at rest (a photo from the user's mail, a venue they are going
/// to): written through `PrivateFile`, filed under sha256 of the account and
/// the card's key so no event id, thread id or place is in a file name, never
/// written for the practice inbox, never painted past `maxAge`, and the
/// account's entries go when the account is removed. A photo is not painted
/// while "load remote images" is off, as it would not be fetched.
@MainActor
final class EventVisualDisk {
    static let shared = EventVisualDisk(
        dir: (FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)
            .first ?? URL(fileURLWithPath: NSTemporaryDirectory()))
            .appendingPathComponent("Passband/EventVisualCache", isDirectory: true))

    private enum Kind: String, Codable { case none, photo, lookAround, map }

    private struct Entry: Codable {
        var kind: Kind
        /// sha256 of the account id, for `forget(account:)`.
        var account: String
        var threadId: String?
        var lat: Double?
        var lon: Double?
        var name: String?
        var address: String?
        var hasImage: Bool
        var storedAt: Double
        var lastAccess: Double
    }

    private struct Index: Codable {
        var v = 1
        var entries: [String: Entry] = [:]
    }

    static let maxEntries = 400
    /// How long an answer may be painted without this session confirming it.
    static let maxAge: TimeInterval = 30 * 86_400
    private static let saveDelay: Duration = .seconds(2)

    private let dir: URL
    private let indexURL: URL
    private let io = DispatchQueue(label: "app.passband.eventvisualdisk", qos: .utility)
    private var index = Index()
    private var loaded = false
    /// Decoded answers, one disk read per key per session. A nil value is a
    /// read that found nothing usable.
    private var decoded: [String: EventVisual??] = [:]
    private var saveTask: Task<Void, Never>?

    init(dir: URL) {
        self.dir = dir
        indexURL = dir.appendingPathComponent("index.json")
    }

    private static func hash(_ s: String) -> String { SHA256.hash(data: Data(s.utf8)).hex }

    /// The file key for a card in the active account; nil with no account,
    /// or in the practice inbox, whose cards are never kept.
    private func fileKey(_ key: String) -> (file: String, account: String)? {
        guard !RehearsalMode.isEnabled, let id = AccountManager.shared.activeId else { return nil }
        return (Self.hash(id.uuidString + "\u{1F}" + key), Self.hash(id.uuidString))
    }

    // MARK: reads

    /// The answer on disk: `.some(nil)` for a card known to have no picture,
    /// nil for one not on disk (or not paintable).
    func visual(_ key: String) -> EventVisual?? {
        guard let (file, _) = fileKey(key) else { return nil }
        if let hit = decoded[file] { return hit }
        let read = decode(file)
        decoded[file] = .some(read)
        return read
    }

    private func decode(_ file: String) -> EventVisual?? {
        ensureLoaded()
        guard let e = index.entries[file] else { return nil }
        guard Date().timeIntervalSince1970 - e.storedAt < Self.maxAge else {
            drop(file)
            return nil
        }
        let image = e.hasImage ? (try? Data(contentsOf: imageURL(file))).flatMap(PlatformImage.init(data:)) : nil
        switch e.kind {
        case .none:
            return .some(nil)
        case .photo:
            guard Prefs.shared.loadRemoteImages, let image, let threadId = e.threadId else { return nil }
            return .some(.photo(image, threadId: threadId))
        case .lookAround, .map:
            guard let item = Self.item(e) else { return nil }
            return .some(e.kind == .map ? .map(item) : .lookAround(nil, item, still: image))
        }
    }

    private static func item(_ e: Entry) -> MKMapItem? {
        guard let lat = e.lat, let lon = e.lon else { return nil }
        let address = e.address.flatMap { MKAddress(fullAddress: $0, shortAddress: nil) }
        let item = MKMapItem(location: CLLocation(latitude: lat, longitude: lon), address: address)
        item.name = e.name
        return item
    }

    // MARK: writes

    /// File what this session resolved. A photo's PNG is written with it; a
    /// Look Around still follows in `storeStill`.
    func store(_ key: String, _ visual: EventVisual?) {
        guard let (file, account) = fileKey(key) else { return }
        ensureLoaded()
        let now = Date().timeIntervalSince1970
        var e = Entry(kind: .none, account: account, hasImage: false, storedAt: now, lastAccess: now)
        var png: Data?
        switch visual {
        case nil:
            break
        case .photo(let image, let threadId):
            e.kind = .photo
            e.threadId = threadId
            png = Self.png(image)
        case .lookAround(_, let item, _), .map(let item):
            if case .map = visual { e.kind = .map } else { e.kind = .lookAround }
            let at = item.location.coordinate
            e.lat = at.latitude
            e.lon = at.longitude
            e.name = item.name
            e.address = item.address?.fullAddress
            // A Look Around entry keeps the still it had until a new one lands.
            if e.kind == .lookAround, index.entries[file]?.kind == .lookAround {
                e.hasImage = index.entries[file]?.hasImage ?? false
            }
        }
        if let png {
            e.hasImage = true
            write(png, file)
        } else if !e.hasImage {
            removeImage(file)
        }
        index.entries[file] = e
        decoded[file] = .some(visual)
        evictIfNeeded()
        scheduleSave()
    }

    func storeStill(_ key: String, png: Data) {
        guard let (file, _) = fileKey(key) else { return }
        ensureLoaded()
        guard index.entries[file]?.kind == .lookAround else { return }
        index.entries[file]?.hasImage = true
        write(png, file)
        scheduleSave()
    }

    /// An account removed: its pictures go with it.
    func forget(account id: UUID) {
        ensureLoaded()
        let account = Self.hash(id.uuidString)
        for (file, e) in index.entries where e.account == account { drop(file) }
    }

    /// An account switch: the decoded answers were the old account's keys.
    func wipeDecoded() { decoded.removeAll() }

    nonisolated static func png(_ image: PlatformImage) -> Data? {
        #if os(macOS)
            guard let cg = image.cgImage(forProposedRect: nil, context: nil, hints: nil) else { return nil }
            return Raster.png(cg)
        #else
            return image.pngData()
        #endif
    }

    // MARK: disk

    private func imageURL(_ file: String) -> URL { dir.appendingPathComponent(file + ".png") }

    private func write(_ png: Data, _ file: String) {
        let url = imageURL(file)
        let dir = dir
        io.async { try? PrivateFile.write(png, to: url, in: dir) }
    }

    private func removeImage(_ file: String) {
        let url = imageURL(file)
        io.async { try? FileManager.default.removeItem(at: url) }
    }

    private func drop(_ file: String) {
        index.entries.removeValue(forKey: file)
        decoded.removeValue(forKey: file)
        removeImage(file)
        scheduleSave()
    }

    private func evictIfNeeded() {
        let over = index.entries.count - Self.maxEntries
        guard over > 0 else { return }
        let oldest = index.entries.sorted { $0.value.lastAccess < $1.value.lastAccess }.prefix(over)
        for (file, _) in oldest { drop(file) }
    }

    private func ensureLoaded() {
        guard !loaded else { return }
        loaded = true
        guard let data = try? Data(contentsOf: indexURL) else { return }
        if let read = try? JSONDecoder().decode(Index.self, from: data), read.v == Index().v {
            index = read
        } else {
            try? FileManager.default.removeItem(at: indexURL)
        }
    }

    private func scheduleSave() {
        guard saveTask == nil else { return }
        saveTask = Task { [weak self] in
            try? await Task.sleep(for: Self.saveDelay)
            self?.saveTask = nil
            self?.saveNow()
        }
    }

    private func saveNow() {
        guard let data = try? JSONEncoder().encode(index) else { return }
        let url = indexURL
        let dir = dir
        io.async { try? PrivateFile.write(data, to: url, in: dir) }
    }
}
