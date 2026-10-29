// The picture on an event's card (Model/EventVisuals.swift chooses it): a
// photo from the mail, Apple's Look Around of the venue, or a map with a pin.
// A strip across the top of the card, cropped to fill, with a small line
// saying where it came from. The photo opens its mail; the map opens Apple
// Maps; Look Around opens its own viewer, as Apple's preview does. Shared by
// the Mac and the phone, like CalendarDetail.

import MapKit
import SwiftUI

struct EventVisualView: View {
    @Environment(AppStore.self) private var store
    let visual: EventVisual

    static let height: CGFloat = 136

    var body: some View {
        content
            .frame(maxWidth: .infinity)
            .frame(height: Self.height)
            .clipShape(RoundedRectangle(cornerRadius: Radius.well, style: .continuous))
            .overlay(alignment: .bottomLeading) { credit }
            .transition(.opacity)
    }

    @ViewBuilder
    private var content: some View {
        switch visual {
        case .photo(let image, let threadId):
            Button {
                store.openThread(threadId, from: .calendar)
            } label: {
                // Mail-supplied pixels, decoded and downsampled already.
                Image(platformImage: image)
                    .resizable()
                    .aspectRatio(contentMode: .fill)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                    .clipped()
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .pointingHand()
        case .lookAround(let scene, _):
            LookAroundPreview(
                initialScene: scene, allowsNavigation: false, showsRoadLabels: false,
                badgePosition: .bottomTrailing)
        case .map(let item):
            Map(initialPosition: .item(item), interactionModes: []) {
                Marker(item: item)
            }
            .mapStyle(.standard(pointsOfInterest: .excludingAll))
            .contentShape(Rectangle())
            .onTapGesture { item.openInMaps() }
            .pointingHand()
        }
    }

    /// Where the picture came from, on a scrim so it reads on any photo.
    @ViewBuilder
    private var credit: some View {
        switch visual {
        case .photo:
            label("From the mail")
        case .lookAround(_, let item), .map(let item):
            Button {
                item.openInMaps()
            } label: {
                label("Open in Maps")
            }
            .buttonStyle(.plain)
            .pointingHand()
        }
    }

    private func label(_ text: String) -> some View {
        Text(text)
            .font(Typo.micro)
            .foregroundStyle(.white)
            .padding(.horizontal, Space.s6)
            .padding(.vertical, Space.s2)
            .background(Capsule().fill(.black.opacity(0.45)))
            .padding(Space.s6)
    }
}
