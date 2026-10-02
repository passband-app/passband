// THE DISCOVERY PAGE, the ring's one door on the rail: a column of rows, one
// per feature set, each a looping picture of the gesture beside its name, one
// line and its keys. `FeatureSet` case order is the column's order, the most
// everyday set first and each one more niche than the last.
//
// TWO SECTIONS, AND THE TOP ONE IS AN INVITATION. What has not been unlocked
// yet sits at the top, large, on a lit card, with its vignette playing: the
// most attractive thing on the page, never the dimmest. No lock, no grey, no
// count of what is left. Below it, under "Unlocked", every set that has been
// found sinks into a calmer, smaller row; a kept one (three separate days)
// is warmer than a merely found one, by fill and a quiet tag. When everything
// is unlocked the top section is simply gone and the masthead says so.
//
// ONE FOREACH FOR BOTH SECTIONS, heading included, so a row moving from the
// top into Unlocked keeps its identity and can be animated across. That is
// what `settled` drives: a set found since the last visit starts where it used
// to sit and settles into Unlocked once the page is up.
//
// MOTION BUDGET. Invitation rows play while on screen. Unlocked rows rest on
// their poster and play on hover; a just-unlocked one plays, because it is the
// news. A row scrolled out of view counts as not live, so its clock stops.
//
// Pure presentation: every input is passed in, so the harness can lay it out
// in any state. `DiscoveryView` is what reads the store and acknowledges.

import SwiftUI

struct DiscoveryPage: View {
    let state: (FeatureSet) -> SetState
    /// Found since the page was last visited: a snapshot taken on arrival,
    /// because arriving is also what clears it in the ledger.
    let fresh: Set<FeatureSet>
    /// False holds every vignette on its poster (window not key).
    var live = true
    /// Pins every vignette's clock, for the harness.
    var fixedTime: Double? = nil
    /// False holds fresh rows in their old place at the top; flipping it is
    /// the arrival.
    var settled = true

    @State private var width: CGFloat = 900
    /// HELD AT THE TOP THROUGH THE ARRIVAL. A scroll view keeps its place by
    /// anchoring to a row on screen, and the row on screen at arrival is the
    /// one that leaves: left alone, the page follows it to the bottom.
    @State private var position = ScrollPosition(edge: .top)

    /// The reading column. Wide enough for a large vignette and a comfortable
    /// line beside it, narrow enough that a row is one glance.
    static let column: CGFloat = 860
    private static let gutter: CGFloat = 32

    /// Below this the invitation rows stack, picture over words, rather than
    /// squeezing either.
    private var stacked: Bool { width < 680 }

    var body: some View {
        ScrollViewReader { proxy in
            ScrollView(.vertical) {
                VStack(alignment: .leading, spacing: 0) {
                    masthead(proxy)
                        .padding(.bottom, 30)
                    ForEach(items, id: \.self) { item in
                        switch item {
                        case .heading:
                            unlockedHeading
                                .transition(.opacity)
                        case .row(let set):
                            let placed = isPlacedUnlocked(set)
                            DiscoveryRow(
                                set: set, state: state(set),
                                prominent: !placed, fresh: placed && fresh.contains(set),
                                stacked: stacked, live: live, fixedTime: fixedTime)
                                .padding(.bottom, placed ? 10 : 18)
                                .id(set)
                        }
                    }
                }
                .padding(.horizontal, Self.gutter)
                .padding(.top, 34)
                .padding(.bottom, 64)
                .frame(maxWidth: Self.column + 2 * Self.gutter, alignment: .leading)
                .frame(maxWidth: .infinity)
            }
            .scrollIndicators(.automatic)
            .scrollPosition($position)
            .onChange(of: settled) { _, now in
                if now { position.scrollTo(edge: .top) }
            }
        }
        .onGeometryChange(for: CGFloat.self) { $0.size.width } action: { width = $0 }
        .accessibilityElement(children: .contain)
        .accessibilityLabel(DiscoveryCopy.ringLabel)
    }

    // MARK: order

    private enum Item: Hashable {
        case row(FeatureSet)
        case heading
    }

    /// In Unlocked by now: found or kept, and (for a fresh one) arrived.
    private func isPlacedUnlocked(_ set: FeatureSet) -> Bool {
        state(set) != .unseen && (settled || !fresh.contains(set))
    }

    private var invitations: [FeatureSet] { FeatureSet.allCases.filter { !isPlacedUnlocked($0) } }
    private var unlocked: [FeatureSet] { FeatureSet.allCases.filter(isPlacedUnlocked) }

    /// Nothing unlocked yet: no Unlocked heading at all. An empty section
    /// would be a shelf waiting to be filled, which is the to-do list again.
    private var items: [Item] {
        invitations.map(Item.row) + (unlocked.isEmpty ? [] : [.heading] + unlocked.map(Item.row))
    }

    private var complete: Bool { FeatureSet.allCases.allSatisfy { state($0) != .unseen } }

    // MARK: masthead

    private func masthead(_ proxy: ScrollViewProxy) -> some View {
        HStack(alignment: .center, spacing: 22) {
            DiscoveryRing(
                progress: DiscoveryProgress(state, fresh: fresh), diameter: 54, breathing: live && fixedTime == nil)
                .frame(width: 78, height: 78)
                .background(Circle().fill(Palette.accentSoft))
                .overlay(Circle().strokeBorder(Palette.accent.opacity(0.25), lineWidth: 0.75))

            VStack(alignment: .leading, spacing: 6) {
                Text(DiscoveryCopy.eyebrow)
                    .font(.system(size: 10.5, weight: .bold).width(.expanded))
                    .tracking(0.7)
                    .foregroundStyle(Palette.accent)
                Text(complete ? DiscoveryCopy.completeHeadline : DiscoveryCopy.headline)
                    .font(Typo.hero(stacked ? 28 : 36))
                    .foregroundStyle(Palette.ink)
                    .fixedSize(horizontal: false, vertical: true)
                Text(complete ? DiscoveryCopy.completeSubline : DiscoveryCopy.subline)
                    .font(.system(size: 14))
                    .foregroundStyle(Palette.inkDim)
                    .lineSpacing(2)
                    .fixedSize(horizontal: false, vertical: true)
                if let newest = FeatureSet.allCases.last(where: { fresh.contains($0) && state($0) != .unseen }) {
                    justUnlockedChip(newest) {
                        withAnimation(.smooth(duration: 0.5)) { proxy.scrollTo(newest, anchor: .center) }
                    }
                    .padding(.top, 6)
                }
            }
        }
    }

    /// FINDABLE AT A GLANCE: the newest find, named in the masthead, and a
    /// click away even when Unlocked is below the fold. A pointer to good
    /// news, not a badge.
    private func justUnlockedChip(_ set: FeatureSet, go: @escaping () -> Void) -> some View {
        Button(action: go) {
            HStack(spacing: 7) {
                Text(DiscoveryCopy.freshTag)
                    .font(.system(size: 9, weight: .bold).width(.expanded))
                    .tracking(0.5)
                    .foregroundStyle(DiscoveryInk.ringHot)
                Text(set.title)
                    .font(.system(size: 12, weight: .medium))
                    .foregroundStyle(Palette.ink)
                Image(systemName: "arrow.down")
                    .font(.system(size: 9.5, weight: .semibold))
                    .foregroundStyle(Palette.inkDim)
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 5)
            .background(Capsule().fill(Palette.accentSoft))
            .overlay(Capsule().strokeBorder(Palette.accent.opacity(0.35), lineWidth: 0.75))
            .contentShape(Capsule())
        }
        .buttonStyle(.plain)
        .accessibilityLabel("Just unlocked: \(set.title). Show it.")
    }

    // MARK: the Unlocked heading

    private var unlockedHeading: some View {
        HStack(alignment: .center, spacing: 14) {
            Text(DiscoveryCopy.unlockedHeading)
                .font(Typo.serif(22, weight: .medium))
                .foregroundStyle(Palette.ink)
                .accessibilityAddTraits(.isHeader)
            Rectangle()
                .fill(Palette.hairline)
                .frame(height: 0.5)
        }
        // More air above than below: the heading belongs to what follows it.
        .padding(.top, complete ? 4 : 26)
        .padding(.bottom, 14)
    }
}

// MARK: - a row

struct DiscoveryRow: View {
    let set: FeatureSet
    let state: SetState
    /// The invitation form: large, lit, playing. False is the Unlocked form.
    let prominent: Bool
    var fresh = false
    /// The narrow window's form: picture over words.
    var stacked = false
    var live = true
    var fixedTime: Double? = nil

    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var hovering = false
    @State private var visible = true

    private var kept: Bool { state == .kept && !prominent }
    private var radius: CGFloat { prominent ? 24 : 18 }

    /// Invitations play; Unlocked rests until hovered, unless it is the news.
    private var playing: Bool { live && visible && (prominent || fresh || hovering) }

    private var vignetteWidth: CGFloat {
        if prominent { return stacked ? 440 : 320 }
        return stacked ? 132 : 176
    }

    var body: some View {
        // ONE LAYOUT VALUE, not an `if`: the stacked and side-by-side forms
        // are the same row, so a window resize does not rebuild it.
        let layout = prominent && stacked
            ? AnyLayout(VStackLayout(alignment: .leading, spacing: 16))
            : AnyLayout(HStackLayout(alignment: .center, spacing: prominent ? 30 : 20))

        layout {
            DiscoveryVignette(
                set: set, live: playing, fixedTime: fixedTime, cornerRadius: prominent ? 14 : 10)
                .frame(maxWidth: vignetteWidth)
                // The screen lit from within: an even phosphor glow all round
                // an invitation's picture, so the top of the page is where the
                // light is.
                .shadow(color: VignetteInk.phosphor.opacity(prominent ? (hovering ? 0.34 : 0.22) : 0), radius: 22)

            VStack(alignment: .leading, spacing: prominent ? 9 : 5) {
                HStack(alignment: .firstTextBaseline, spacing: 10) {
                    Text(set.title)
                        .font(prominent ? Typo.hero(30) : Typo.serif(21, weight: .medium))
                        .foregroundStyle(Palette.ink)
                    tag
                }
                Text(set.line)
                    .font(.system(size: prominent ? 14.5 : 12.5))
                    .foregroundStyle(Palette.inkDim)
                    .lineSpacing(prominent ? 3 : 1.5)
                    .fixedSize(horizontal: false, vertical: true)
                    .frame(maxWidth: 400, alignment: .leading)
                HStack(spacing: 5) {
                    ForEach(set.keys, id: \.self) { DiscoveryKeycap($0, large: prominent) }
                }
                .padding(.top, prominent ? 8 : 4)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .padding(prominent ? 16 : 10)
        .padding(.trailing, prominent ? 16 : 10)
        .background { card }
        .overlay {
            // EVEN all round: a state is a border's colour and weight, never
            // one lit edge.
            RoundedRectangle(cornerRadius: radius, style: .continuous)
                .strokeBorder(border, lineWidth: fresh || kept ? 1 : 0.75)
        }
        .shadow(color: fresh ? Palette.accent.opacity(0.35) : .clear, radius: 14)
        .contentShape(RoundedRectangle(cornerRadius: radius, style: .continuous))
        .onHover { hovering = $0 }
        .animation(reduceMotion ? nil : .smooth(duration: 0.2), value: hovering)
        // SCROLLED AWAY IS NOT LIVE. The stage stops on disappear, but rows in
        // a plain VStack never disappear, so the scroll view is asked.
        .onScrollVisibilityChange(threshold: 0.05) { visible = $0 }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("\(set.title), \(set.stateWords(state)). \(set.line)")
        .accessibilityHint("Keys: \(set.keys.joined(separator: ", "))")
    }

    @ViewBuilder private var card: some View {
        let shape = RoundedRectangle(cornerRadius: radius, style: .continuous)
        ZStack {
            shape.fill(fill)
            // The invitation's light: a still, wide bloom from behind the
            // picture, the same phosphor the vignettes are lit with. It is
            // what makes the top of the page the warm part of it.
            shape.fill(
                RadialGradient(
                    colors: [Palette.accent.opacity(prominent ? (hovering ? 0.2 : 0.14) : 0), .clear],
                    center: stacked ? UnitPoint(x: 0.5, y: 0.3) : UnitPoint(x: 0.28, y: 0.5),
                    startRadius: 0, endRadius: 520))
        }
    }

    private var fill: Color {
        if prominent { return Palette.readerBackground.opacity(hovering ? 0.75 : 0.6) }
        if kept { return Palette.accentSoft.opacity(hovering ? 1 : 0.8) }
        return Palette.hairline.opacity(hovering ? 0.5 : 0.28)
    }

    private var border: Color {
        if fresh { return DiscoveryInk.ringHot.opacity(0.7) }
        if prominent { return hovering ? Palette.accent.opacity(0.35) : Palette.hairline }
        if kept { return Palette.accent.opacity(0.45) }
        return Palette.hairline
    }

    /// Kept and just-unlocked wear a tag beside the name; plain unlocked and
    /// the invitations wear nothing: the section already says what they are,
    /// and "not yet" is not a state worth a label.
    private var tag: some View {
        let word = fresh ? DiscoveryCopy.freshTag : DiscoveryCopy.keptTag
        return Text(word)
            .font(.system(size: 8.5, weight: .bold).width(.expanded))
            .tracking(0.5)
            .foregroundStyle(kept && !fresh ? Color.white : DiscoveryInk.ringHot)
            .padding(.horizontal, 7)
            .padding(.vertical, 3)
            .background(Capsule().fill(kept && !fresh ? Palette.accent : Palette.accentSoft))
            .fixedSize()
            // Opacity, not an `if`: the tag comes and goes as the row settles.
            .opacity(!prominent && (fresh || kept) ? 1 : 0)
            .alignmentGuide(.firstTextBaseline) { d in d[VerticalAlignment.center] + 3 }
    }
}

// MARK: - keycaps

/// The page's keycap: a touch more of a KEY than the app's inline `Kbd`, since
/// here the key is the lesson. Even border and a soft drop shadow beneath, never
/// a lit top edge.
struct DiscoveryKeycap: View {
    let label: String
    var large = false
    init(_ label: String, large: Bool = false) {
        self.label = label
        self.large = large
    }

    var body: some View {
        Text(label)
            .font(Typo.mono(large ? 12 : 11, weight: .medium))
            .foregroundStyle(Palette.ink.opacity(0.85))
            .padding(.horizontal, large ? 8 : 7)
            .frame(minWidth: large ? 25 : 22, minHeight: large ? 24 : 21)
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
/// rather than just naming it. Self-dismissing; a click opens the page.
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
                    // One arc lit at twelve, glowing: the ring's own picture
                    // of "the ring just grew".
                    DiscoveryRingGlyph(
                        progress: DiscoveryProgress(kept: 0, found: 1, fresh: 1), diameter: 10, glow: 0.9)
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
