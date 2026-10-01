// THE DISCOVERY MENU, opened from the ring on the rail: one tile per feature
// set, each a looping picture of the gesture, its name, one line and its keys.
//
// A FIELD GUIDE, NOT A TROPHY CASE. A set nobody has found yet is the most
// inviting tile on the card, not the dimmest: its vignette plays like the
// others, its name is as large, and it carries no lock, no grey, no "0/3", no
// checkbox. Found and kept are told apart by warmth (a tag and a lit border),
// never by greying out what is left.
//
// IT GROWS OUT OF THE RING. Anchored beside the rail at the ring's height and
// scaled in from its bottom-leading corner, so it reads as the ring opening
// rather than as a dialog arriving from nowhere. A light dim, no blur: the
// board stays visible behind it, and clicking the dim closes it.
//
// Pure presentation: every input is passed in, so the harness can lay it out
// in any state. `DiscoveryLayer` is what mounts it, owns Esc and acknowledges.

import SwiftUI

struct DiscoveryMenu: View {
    let state: (FeatureSet) -> SetState
    let fresh: Set<FeatureSet>
    /// False holds every vignette on its poster (window not key).
    var live = true
    /// Pins every vignette's clock, for the harness.
    var fixedTime: Double? = nil
    /// Four across when the window has the room, two when it does not (the
    /// layer decides from the space it has): a menu that scrolls sideways is
    /// a menu nobody finishes reading.
    var columns = 4
    let onClose: () -> Void

    static let tileWidth: CGFloat = 178
    static let spacing: CGFloat = 12
    static let inset: CGFloat = 20
    static let radius: CGFloat = 24

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            header.frame(width: Self.gridWidth(columns: columns))
            grid(columns: columns)
        }
        .padding(Self.inset)
        .fixedSize()
        .background {
            // The ring's light, behind the emblem: still, cheap, and the one
            // place the card's glass is coloured.
            RadialGradient(
                colors: [Palette.accent.opacity(0.22), .clear],
                center: UnitPoint(x: 0.06, y: 0.0), startRadius: 0, endRadius: 360)
                .allowsHitTesting(false)
        }
        .passbandGlass(.pane, cornerRadius: Self.radius, tint: Palette.glassTintStrong)
        .shadow(color: .black.opacity(0.32), radius: 36, y: 16)
        .accessibilityElement(children: .contain)
        .accessibilityLabel(DiscoveryCopy.ringLabel)
    }

    static func gridWidth(columns: Int) -> CGFloat {
        CGFloat(columns) * tileWidth + CGFloat(columns - 1) * spacing
    }

    /// The whole card's width at a column count, for the layer's fit check.
    static func width(columns: Int) -> CGFloat { gridWidth(columns: columns) + 2 * inset }

    private var header: some View {
        HStack(alignment: .center, spacing: 16) {
            DiscoveryRing(
                states: FeatureSet.ringStates(state), fresh: fresh, diameter: 46, breathing: live)
                .frame(width: 58, height: 58)
                .background(Circle().fill(Palette.accentSoft))
                .overlay(Circle().strokeBorder(Palette.accent.opacity(0.25), lineWidth: 0.75))

            VStack(alignment: .leading, spacing: 4) {
                Text(DiscoveryCopy.eyebrow)
                    .font(.system(size: 10, weight: .bold).width(.expanded))
                    .tracking(0.6)
                    .foregroundStyle(Palette.accent)
                Text(DiscoveryCopy.headline)
                    .font(Typo.hero(26))
                    .foregroundStyle(Palette.ink)
                Text(DiscoveryCopy.subline)
                    .font(Typo.rowSub)
                    .foregroundStyle(Palette.inkDim)
            }

            Spacer(minLength: 12)

            Button(action: onClose) {
                Image(systemName: "xmark")
                    .font(.system(size: 10, weight: .bold))
                    .foregroundStyle(Palette.inkDim)
                    .frame(width: 26, height: 26)
                    .background(Palette.hairline, in: Circle())
                    .contentShape(Circle())
            }
            .buttonStyle(.plain)
            .help("Close (Esc)")
            .accessibilityLabel("Close")
            .frame(maxHeight: .infinity, alignment: .top)
        }
        .fixedSize(horizontal: false, vertical: true)
    }

    private func grid(columns: Int) -> some View {
        let rows = stride(from: 0, to: FeatureSet.allCases.count, by: columns).map {
            Array(FeatureSet.allCases[$0..<min($0 + columns, FeatureSet.allCases.count)])
        }
        return VStack(alignment: .leading, spacing: Self.spacing) {
            ForEach(rows.indices, id: \.self) { r in
                HStack(alignment: .top, spacing: Self.spacing) {
                    ForEach(rows[r], id: \.self) { set in
                        DiscoveryTile(
                            set: set, state: state(set), fresh: fresh.contains(set),
                            live: live, fixedTime: fixedTime)
                    }
                }
                // Each row as tall as its tallest tile, so the keycaps line up.
                .fixedSize(horizontal: false, vertical: true)
            }
        }
    }
}

// MARK: - a tile

struct DiscoveryTile: View {
    let set: FeatureSet
    let state: SetState
    var fresh = false
    var live = true
    var fixedTime: Double? = nil

    @State private var hovering = false

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            DiscoveryVignette(set: set, live: live, fixedTime: fixedTime, cornerRadius: 11)
                // The vignette language keeps this corner clear for it.
                .overlay(alignment: .bottomLeading) { tag.padding(6) }

            VStack(alignment: .leading, spacing: 5) {
                Text(set.title)
                    .font(Typo.serif(18, weight: .medium))
                    .foregroundStyle(Palette.ink)
                Text(set.line)
                    .font(Typo.rowSub)
                    .foregroundStyle(Palette.inkDim)
                    .lineSpacing(1.5)
                    .fixedSize(horizontal: false, vertical: true)
            }
            .padding(.top, 11)

            Spacer(minLength: 12)

            HStack(spacing: 5) {
                ForEach(set.keys, id: \.self) { DiscoveryKeycap($0) }
            }
        }
        .padding(9)
        .padding(.bottom, 3)
        .frame(width: DiscoveryMenu.tileWidth, alignment: .leading)
        .frame(maxHeight: .infinity, alignment: .top)
        .background {
            RoundedRectangle(cornerRadius: 16, style: .continuous)
                .fill(fill)
        }
        .overlay {
            // EVEN all round: a state is a border's colour and weight, never
            // one lit edge.
            RoundedRectangle(cornerRadius: 16, style: .continuous)
                .strokeBorder(border, lineWidth: state == .kept ? 1.25 : 0.75)
        }
        .shadow(color: state == .kept ? Palette.accent.opacity(0.28) : .clear, radius: 12)
        .scaleEffect(hovering ? 1.015 : 1)
        .animation(.smooth(duration: 0.18), value: hovering)
        .contentShape(RoundedRectangle(cornerRadius: 16, style: .continuous))
        .onHover { hovering = $0 }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("\(set.title), \(set.stateWords(state)). \(set.line)")
        .accessibilityHint("Keys: \(set.keys.joined(separator: ", "))")
    }

    private var fill: Color {
        switch state {
        case .unseen: Palette.hairline.opacity(hovering ? 0.55 : 0.35)
        case .found: Palette.accentSoft.opacity(hovering ? 0.75 : 0.5)
        case .kept: Palette.accentSoft.opacity(hovering ? 1.15 : 0.95)
        }
    }

    private var border: Color {
        switch state {
        case .unseen: Palette.hairline
        case .found: Palette.accent.opacity(0.35)
        case .kept: Palette.accent.opacity(0.7)
        }
    }

    /// Found and kept wear a tag; unseen wears nothing, because "not yet" is
    /// not a state worth a label.
    private var tag: some View {
        HStack(spacing: 5) {
            DiscoveryRingGlyph(
                states: FeatureSet.ringStates { $0 == set ? state : .unseen },
                fresh: fresh ? [set] : [], only: set, diameter: 10, glow: 0.8,
                ink: state == .kept ? .white : nil)
                .frame(width: 12, height: 12)
            Text(state == .kept ? "A HABIT" : "FOUND")
                .font(.system(size: 8.5, weight: .bold).width(.expanded))
                .tracking(0.5)
                .foregroundStyle(.white.opacity(0.95))
        }
        .padding(.leading, 4)
        .padding(.trailing, 8)
        .padding(.vertical, 3)
        // On the vignette's night ground in both themes, so it takes the
        // ground's colours rather than the tile's.
        .background(Capsule().fill(state == .kept ? VignetteInk.phosphor : .black.opacity(0.45)))
        .overlay(Capsule().strokeBorder(VignetteInk.phosphorHot.opacity(state == .kept ? 0 : 0.45), lineWidth: 0.5))
        .environment(\.colorScheme, .dark)
        .fixedSize()
        .opacity(state == .unseen ? 0 : 1)
    }
}

// MARK: - keycaps

/// The menu's keycap: a touch more of a KEY than the app's inline `Kbd`, since
/// here the key is the lesson. Even border and a soft drop shadow beneath, never
/// a lit top edge.
struct DiscoveryKeycap: View {
    let label: String
    init(_ label: String) { self.label = label }

    var body: some View {
        Text(label)
            .font(Typo.mono(11, weight: .medium))
            .foregroundStyle(Palette.ink.opacity(0.85))
            .padding(.horizontal, 7)
            .frame(minWidth: 22, minHeight: 21)
            .background(
                RoundedRectangle(cornerRadius: 5, style: .continuous)
                    .fill(Palette.readerBackground.opacity(0.85))
                    .shadow(color: .black.opacity(0.14), radius: 0.5, y: 1)
            )
            .overlay(
                RoundedRectangle(cornerRadius: 5, style: .continuous)
                    .strokeBorder(Palette.hairlineStrong, lineWidth: 0.75)
            )
            .fixedSize()
    }
}

// MARK: - the found moment

/// SOMETHING NEW: a small card beside the ring when a set is found, at rest.
/// The set's own vignette on the left, so the moment shows the gesture again
/// rather than just naming it. Self-dismissing; a click opens the menu.
struct DiscoveryMoment: View {
    let set: FeatureSet
    var live = true
    var fixedTime: Double? = nil

    var body: some View {
        HStack(spacing: 13) {
            DiscoveryVignette(set: set, live: live, fixedTime: fixedTime, cornerRadius: 9)
                .frame(width: 98)

            VStack(alignment: .leading, spacing: 3) {
                HStack(spacing: 5) {
                    DiscoveryRingGlyph(
                        states: FeatureSet.ringStates { $0 == set ? .found : .unseen },
                        fresh: [set], only: set, diameter: 10, glow: 0.9)
                        .frame(width: 12, height: 12)
                    Text(DiscoveryCopy.momentEyebrow)
                        .font(.system(size: 9, weight: .bold).width(.expanded))
                        .tracking(0.6)
                        .foregroundStyle(Palette.accent)
                }
                Text(set.foundTitle)
                    .font(Typo.serif(18, weight: .medium))
                    .foregroundStyle(Palette.ink)
                    .fixedSize(horizontal: false, vertical: true)
                Text(set.foundLine)
                    .font(Typo.rowSub)
                    .foregroundStyle(Palette.inkDim)
                    .fixedSize(horizontal: false, vertical: true)
            }
            .frame(width: 204, alignment: .leading)
        }
        .padding(9)
        .padding(.trailing, 7)
        .passbandGlass(.pane, cornerRadius: 18, tint: Palette.glassTintStrong)
        .shadow(color: .black.opacity(0.26), radius: 22, y: 10)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("\(set.foundTitle) \(set.foundLine)")
    }
}
