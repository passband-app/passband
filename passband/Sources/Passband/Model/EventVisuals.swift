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

import Foundation
import MapKit

/// What an event card shows above its title.
enum EventVisual {
    case photo(PlatformImage, threadId: String)
    case lookAround(MKLookAroundScene, MKMapItem)
    case map(MKMapItem)
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

    /// What is known already, without starting any work: `.some(nil)` is
    /// "nothing to show", `nil` is "not looked yet".
    func cached(_ key: String) -> EventVisual?? { memo[key] }

    func wipe() {
        memo.removeAll()
        order.removeAll()
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
        remember(key, found)
        return found
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
                EventVisualPick.venueMatches(place: place, name: $0.name)
            })
        else { return nil }
        if let scene = try? await MKLookAroundSceneRequest(mapItem: item).scene {
            return .lookAround(scene, item)
        }
        return .map(item)
    }
}
