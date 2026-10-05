// THE DISCOVERY PAGE, the ring's one door on the rail: a column of rows, one
// per feature set, each a looping picture of the gesture beside its name, one
// line and its keys. `FeatureSet` case order is the column's order, the most
// everyday set first and each one more niche than the last.
//
// TWO SECTIONS, AND THE TOP ONE IS AN INVITATION. What has not been unlocked
// yet sits at the top, a size up from the rest: the first thing read, never
// the dimmest. No lock, no grey, no count of what is left. Below it, under
// "Unlocked", every set that has been found sinks into a smaller row; a kept
// one (three separate days) is warmer than a merely found one, by fill and a
// quiet tag. When everything is unlocked the top section is simply gone and
// the masthead says so.
//
// QUIET ON PURPOSE. No glow, no bloom, no light behind anything: a row is a
// flat card with a hairline, and the page is a list, not a trophy case.
//
// ONE FOREACH FOR BOTH SECTIONS, heading included, so a row moving from the
// top into Unlocked keeps its identity and can be animated across. That is
// what `settled` drives: a set found since the last visit starts where it used
// to sit and settles into Unlocked once the page is up.
//
// MOTION BUDGET. Every row rests on its poster and plays only under the
// pointer: one picture moving at a time, and only the one being looked at.
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

    /// The reading column. Wide enough for a vignette and a comfortable line
    /// beside it, narrow enough that a row is one glance.
    static let column: CGFloat = 720
    private static let gutter: CGFloat = 32

    /// Below this the invitation rows stack, picture over words, rather than
    /// squeezing either.
    private var stacked: Bool { width < 520 }

    var body: some View {
        ScrollViewReader { proxy in
            ScrollView(.vertical) {
                VStack(alignment: .leading, spacing: 0) {
                    masthead(proxy)
                        .padding(.bottom, 22)
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
                                .padding(.bottom, 8)
                                .id(set)
                        }
                    }
                }
                .padding(.horizontal, Self.gutter)
                .padding(.top, 26)
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
        HStack(alignment: .center, spacing: 16) {
            DiscoveryRing(
                progress: DiscoveryProgress(state, fresh: fresh), diameter: 40, breathing: live && fixedTime == nil)
                .frame(width: 44, height: 44)

            VStack(alignment: .leading, spacing: 4) {
                Text(DiscoveryCopy.eyebrow)
                    .font(.system(size: 10.5, weight: .bold).width(.expanded))
                    .tracking(0.7)
                    .foregroundStyle(Palette.accent)
                Text(complete ? DiscoveryCopy.completeHeadline : DiscoveryCopy.headline)
                    .font(Typo.serif(stacked ? 22 : 26, weight: .medium))
                    .foregroundStyle(Palette.ink)
                    .fixedSize(horizontal: false, vertical: true)
                Text(complete ? DiscoveryCopy.completeSubline : DiscoveryCopy.subline)
                    .font(.system(size: 13))
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
                .font(Typo.serif(17, weight: .medium))
                .foregroundStyle(Palette.inkDim)
                .accessibilityAddTraits(.isHeader)
            Rectangle()
                .fill(Palette.hairline)
                .frame(height: 0.5)
        }
        // More air above than below: the heading belongs to what follows it.
        .padding(.top, complete ? 4 : 18)
        .padding(.bottom, 12)
    }
}

// MARK: - a row

struct DiscoveryRow: View {
    let set: FeatureSet
    let state: SetState
    /// The invitation form, a size up. False is the Unlocked form.
    let prominent: Bool
    var fresh = false
    /// The narrow window's form: picture over words.
    var stacked = false
    var live = true
    var fixedTime: Double? = nil

    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var hovering = false

    private var kept: Bool { state == .kept && !prominent }
    private var radius: CGFloat { prominent ? 16 : 13 }

    /// ONLY UNDER THE POINTER. Everything else holds its poster.
    private var playing: Bool { live && hovering }

    private var vignetteWidth: CGFloat {
        if prominent { return stacked ? 300 : 168 }
        return 112
    }

    var body: some View {
        // ONE LAYOUT VALUE, not an `if`: the stacked and side-by-side forms
        // are the same row, so a window resize does not rebuild it.
        let layout = prominent && stacked
            ? AnyLayout(VStackLayout(alignment: .leading, spacing: 12))
            : AnyLayout(HStackLayout(alignment: .center, spacing: prominent ? 18 : 14))

        layout {
            DiscoveryVignette(
                set: set, live: playing, fixedTime: fixedTime, cornerRadius: prominent ? 9 : 7)
                .frame(maxWidth: vignetteWidth)

            VStack(alignment: .leading, spacing: prominent ? 4 : 2) {
                HStack(alignment: .firstTextBaseline, spacing: 10) {
                    Text(set.title)
                        .font(Typo.serif(prominent ? 19 : 15.5, weight: .medium))
                        .foregroundStyle(Palette.ink)
                    tag
                }
                Text(set.line)
                    .font(.system(size: prominent ? 12.5 : 12))
                    .foregroundStyle(Palette.inkDim)
                    .lineSpacing(1.5)
                    .fixedSize(horizontal: false, vertical: true)
                    .frame(maxWidth: 420, alignment: .leading)
                HStack(spacing: 5) {
                    ForEach(set.keys, id: \.self) { DiscoveryKeycap($0) }
                }
                .padding(.top, prominent ? 6 : 4)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .padding(prominent ? 9 : 7)
        .padding(.trailing, 9)
        .background { RoundedRectangle(cornerRadius: radius, style: .continuous).fill(fill) }
        .overlay {
            // EVEN all round: a state is a border's colour and weight, never
            // one lit edge.
            RoundedRectangle(cornerRadius: radius, style: .continuous)
                .strokeBorder(border, lineWidth: 0.75)
        }
        .contentShape(RoundedRectangle(cornerRadius: radius, style: .continuous))
        .onHover { hovering = $0 }
        .animation(reduceMotion ? nil : .smooth(duration: 0.2), value: hovering)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("\(set.title), \(set.stateWords(state)). \(set.line)")
        .accessibilityHint("Keys: \(set.keys.joined(separator: ", "))")
    }

    private var fill: Color {
        if kept { return Palette.accentSoft.opacity(hovering ? 0.75 : 0.5) }
        if prominent { return Palette.hairline.opacity(hovering ? 0.6 : 0.34) }
        return Palette.hairline.opacity(hovering ? 0.4 : 0.16)
    }

    private var border: Color {
        if fresh { return DiscoveryInk.ringHot.opacity(0.6) }
        if kept { return Palette.accent.opacity(0.3) }
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
            .foregroundStyle(fresh ? DiscoveryInk.ringHot : Palette.accent)
            .padding(.horizontal, 7)
            .padding(.vertical, 3)
            .background(Capsule().fill(Palette.accentSoft))
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
