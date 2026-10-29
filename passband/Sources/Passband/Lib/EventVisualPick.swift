// WHICH PICTURE AN EVENT'S CARD WEARS, as rules with no I/O in them
// (Tests/EventVisualPickTests.swift). The fetching is Model/EventVisuals.swift.
//
// The order, best first:
//   1. A PHOTO FROM THE MAIL BEHIND IT: the restaurant's dining room in a
//      reservation, the poster in a ticket, the hotel in a booking. Only a
//      photo-shaped image counts; a logo or a thin banner does not.
//   2. THE VENUE, from Apple Maps: its Look Around street view, or a map with
//      a pin where Apple has no street view. Only when Apple's answer is the
//      place the event names, never a lookalike in another city.
//   3. Nothing. A card with no picture is better than a wrong one.

import Foundation

enum EventVisualPick {
    /// How many images of one message are tried for a photo. The first is
    /// usually the sender's logo, so one is not enough; each try is a fetch.
    static let photoCandidates = 4

    /// Is this image photo-shaped? Big enough to fill the card at 2x, and
    /// neither a strip (a banner, a divider) nor a tower. A square logo at
    /// photo size gets through; the card crops it like any photo.
    static func isPhoto(width: Int, height: Int) -> Bool {
        guard width >= 360, height >= 180 else { return false }
        let aspect = Double(width) / Double(height)
        return aspect >= 0.75 && aspect <= 3.2
    }

    /// Words that make a location a call, not somewhere to go.
    private static let virtual = [
        "zoom", "google meet", "meet.google", "teams", "webex", "facetime", "hangout",
        "phone", "call", "online", "virtual", "tbd", "tba",
    ]

    /// Is this place worth asking Apple Maps about? Not a link, not a call,
    /// and with a letter in it.
    static func wantsVenue(_ place: String) -> Bool {
        let text = place.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        guard text.count >= 3, text.contains(where: \.isLetter), !text.contains("/") else {
            return false
        }
        let words = Set(text.split(whereSeparator: { !$0.isLetter && !$0.isNumber }).map(String.init))
        return !virtual.contains { term in
            term.contains(" ") || term.contains(".") ? text.contains(term) : words.contains(term)
        }
    }

    /// Is Apple's answer the place the event names? Its name (or, for an
    /// address, its first line) and the event's first comma part must share
    /// their words: every word of the shorter is in the longer. "Dingles
    /// Public House" names "Dingles Public House, 165 Main St"; "Dingle Pub"
    /// does not.
    ///
    /// When the place says where it is (anything after the first comma), the
    /// answer's address must share a word of it too, so a chain's name does
    /// not match its branch in another city. Street words ("st", "ave") do not
    /// count: every address has one.
    static func venueMatches(place: String, name: String?, address: String? = nil) -> Bool {
        guard let name, !name.isEmpty else { return false }
        let parts = place.split(separator: ",", maxSplits: 1).map(String.init)
        let head = parts.first ?? place
        let a = words(head)
        let b = words(name)
        guard !a.isEmpty, !b.isEmpty else { return false }
        guard a.count <= b.count ? a.isSubset(of: b) : b.isSubset(of: a) else { return false }
        guard parts.count > 1 else { return true }
        let wanted = words(parts[1]).subtracting(streetWords)
        if wanted.isEmpty { return true }
        guard let address else { return false }
        return !wanted.isDisjoint(with: words(address))
    }

    private static let streetWords: Set<String> = [
        "st", "street", "ave", "avenue", "rd", "road", "blvd", "boulevard", "dr", "drive",
        "ln", "lane", "way", "pl", "place", "ct", "court", "hwy", "suite", "ste", "floor",
        "fl", "unit", "n", "s", "e", "w", "ne", "nw", "se", "sw", "usa", "us",
    ]

    private static func words(_ text: String) -> Set<String> {
        let stop: Set<String> = ["the", "a", "an", "and", "of", "at", "&"]
        return Set(
            text.lowercased()
                .folding(options: .diacriticInsensitive, locale: nil)
                .split(whereSeparator: { !$0.isLetter && !$0.isNumber })
                .map(String.init)
                .filter { !stop.contains($0) })
    }
}
