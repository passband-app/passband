// The rules behind an event card's picture (Lib/EventVisualPick.swift), and
// the image candidates it is offered from the mail (Lib/Trackers.swift).
//
// Each rule's failure is a confident wrong picture: a logo blown up as a
// photo, a map of a Zoom call, a pub with a similar name in another city.

import Foundation

@main
struct EventVisualPickTests {
    nonisolated(unsafe) static var failures = 0
    nonisolated(unsafe) static var checks = 0

    static func main() {
        onlyPhotoShapedImagesAreAPhoto()
        onlyAPlaceToGoIsLookedUp()
        applesAnswerMustBeThePlaceNamed()
        theMailOffersEveryPhotoInOrder()

        if failures > 0 {
            print("FAILED: \(failures) of \(checks) checks")
            exit(1)
        }
        print("ok: \(checks) checks passed")
    }

    static func expect(_ condition: Bool, _ label: String) {
        checks += 1
        if !condition {
            failures += 1
            print("FAIL: \(label)")
        }
    }

    static func onlyPhotoShapedImagesAreAPhoto() {
        expect(EventVisualPick.isPhoto(width: 1200, height: 600), "a wide photo")
        expect(EventVisualPick.isPhoto(width: 600, height: 600), "a square photo")
        expect(!EventVisualPick.isPhoto(width: 600, height: 80), "a banner")
        expect(!EventVisualPick.isPhoto(width: 200, height: 120), "a logo")
        expect(!EventVisualPick.isPhoto(width: 400, height: 1200), "a tower")
    }

    static func onlyAPlaceToGoIsLookedUp() {
        expect(EventVisualPick.wantsVenue("Brambles Public House"), "a venue")
        expect(EventVisualPick.wantsVenue("165 Main St, Brooklyn"), "an address")
        expect(!EventVisualPick.wantsVenue("zoom.us/j/123"), "a meeting link")
        expect(!EventVisualPick.wantsVenue("Zoom"), "a call")
        expect(!EventVisualPick.wantsVenue("Google Meet"), "a call, two words")
        expect(!EventVisualPick.wantsVenue("Phone call"), "a phone call")
        expect(!EventVisualPick.wantsVenue("42"), "no letters")
        expect(EventVisualPick.wantsVenue("Callaghan's Bar"), "a word that only starts like a call")
    }

    static func applesAnswerMustBeThePlaceNamed() {
        expect(
            EventVisualPick.venueMatches(place: "Brambles Public House", name: "Brambles Public House"),
            "the same name")
        expect(
            EventVisualPick.venueMatches(
                place: "Brambles Public House, 88 Orchard Ave", name: "Brambles Public House",
                address: "88 Orchard Ave, Fairview, OR 97024"),
            "the name before the address")
        expect(EventVisualPick.venueMatches(place: "The Grill", name: "Grill"), "small words are free")
        expect(
            EventVisualPick.venueMatches(place: "Café Mogador", name: "Cafe Mogador"),
            "accents are free")
        expect(
            !EventVisualPick.venueMatches(place: "Brambles Public House", name: "Bramble Pub"),
            "a lookalike")
        expect(!EventVisualPick.venueMatches(place: "Brambles Public House", name: nil), "no name")
        expect(
            EventVisualPick.venueMatches(
                place: "Starbucks, 12 Main St, Portland ME", name: "Starbucks",
                address: "12 Main St, Portland, ME 04101, United States"),
            "the branch at the address")
        expect(
            !EventVisualPick.venueMatches(
                place: "Starbucks, 12 Main St, Portland ME", name: "Starbucks",
                address: "1912 Pike Pl, Seattle, WA 98101, United States"),
            "a chain's branch in another city")
        expect(
            !EventVisualPick.venueMatches(
                place: "Starbucks, 12 Main St, Portland ME", name: "Starbucks", address: nil),
            "an address to check and none to check it against")
        expect(
            EventVisualPick.venueMatches(
                place: "Brambles Public House, Suite", name: "Brambles Public House", address: nil),
            "only street words after the comma: nothing to check")
    }

    static func theMailOffersEveryPhotoInOrder() {
        let html = """
            <img src="https://x.example/logo.png" width="60" height="30">
            <img src="https://x.example/brand.png" width="200" height="80">
            <img src="//cdn.example/room.jpg">
            <img src="cid:inline">
            <img src="https://cdn.example/room.jpg">
            <img src="https://cdn.example/food.jpg">
            """
        expect(
            Trackers.extractImageSrcs(html, limit: 4)
                == [
                    "https://x.example/brand.png", "https://cdn.example/room.jpg",
                    "https://cdn.example/food.jpg",
                ],
            "candidates in order, deduplicated, chrome skipped")
        expect(
            Trackers.extractImageSrcs(html, limit: 1) == ["https://x.example/brand.png"],
            "the limit holds")
        expect(Trackers.extractImageSrcs(html, limit: 0).isEmpty, "a zero limit asks for none")
    }
}
