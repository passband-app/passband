// THE DISCOVERY PAGE, the ring's one door on the rail: a table of contents,
// one line per feature set. `FeatureSet` case order is the column's order, the
// most everyday set first and each one more niche than the last.
//
// A LIST, NOT A SHELF. No cards, no borders, no glow, no keycaps: a small still
// of the gesture, the set's name and one line, hairlines between. The detail
// (the keys, the steps, the picture large and moving) is the guide's, one click
// away, so the list only has to be easy to run an eye down.
//
// ONE PLACE TO START. The first set not yet unlocked is the page's one card:
// its picture large, its line and its keys, under "Start here" (or "Try this
// next" once a few are unlocked). Everything else is a name beside a small
// still, so the page reads as one suggestion and a short index, not a list of
// seven things to get through.
//
// TWO SECTIONS. What has not been unlocked yet is on top: no lock, no grey, no
// count of what is left. Under "Unlocked" are the sets that have been found,
// in the same row; a kept one (three separate days) and a just
// unlocked one say so in a word beside the name. When everything is unlocked
// the top section is simply gone.
//
// ONE FOREACH FOR BOTH SECTIONS, heading included, so a row moving from the
// top into Unlocked keeps its identity and can be animated across. That is
// what `settled` drives: a set found since the last visit starts where it used
// to sit and settles into Unlocked once the page is up.
//
// MOTION BUDGET. Every row rests on its poster and plays only under the
// pointer: one picture moving at a time, and only the one being looked at.
//
// ALWAYS TWO HALVES. The list is the left half and a guide is always open in
// the right: the picture large and playing, then the gesture step by step. The
// page opens on the first set not yet unlocked, and clicking a row swaps the
// guide. There is no list-only state, so nothing slides or re-wraps. (The
// window's minimum width leaves room for both; there is no narrow form.)
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
    /// The set whose guide is open. There is always one.
    @Binding var selection: FeatureSet

    @State private var width: CGFloat = 900
    /// HELD AT THE TOP THROUGH THE ARRIVAL. A scroll view keeps its place by
    /// anchoring to a row on screen, and the row on screen at arrival is the
    /// one that leaves: left alone, the page follows it to the bottom.
    @State private var position = ScrollPosition(edge: .top)

    /// The column's widest measure: a name and a line, read in one glance.
    static let column: CGFloat = 520
    private static let gutter: CGFloat = 24

    private var listWidth: CGFloat {
        max(0, min(Self.column, width / 2 - 2 * Self.gutter))
    }

    var body: some View {
        HStack(spacing: 0) {
            list
                .frame(width: width / 2)
            Rectangle().fill(Palette.hairline).frame(width: 0.5)
            DiscoveryGuide(set: selection, state: state(selection), live: live, fixedTime: fixedTime)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
        .onGeometryChange(for: CGFloat.self) { $0.size.width } action: { width = $0 }
    }

    /// WHERE THE PAGE OPENS: the most everyday set not yet unlocked, or the
    /// first of all once everything is.
    static func opening(_ state: (FeatureSet) -> SetState) -> FeatureSet {
        FeatureSet.allCases.first { state($0) == .unseen } ?? FeatureSet.allCases[0]
    }

    private var list: some View {
        ScrollView(.vertical) {
            VStack(alignment: .leading, spacing: 0) {
                masthead
                    .padding(.horizontal, DiscoveryRow.inset)
                    .padding(.bottom, 14)
                ForEach(items, id: \.self) { item in
                    switch item {
                    case .heading:
                        heading(DiscoveryCopy.unlockedHeading, top: complete ? 4 : 22)
                            .transition(.opacity)
                    case .more:
                        heading(DiscoveryCopy.moreHeading, top: 20)
                            .transition(.opacity)
                    case .row(let set):
                        let placed = isPlacedUnlocked(set)
                        DiscoveryRow(
                            set: set, state: state(set),
                            unlocked: placed, fresh: placed && fresh.contains(set),
                            featured: set == featured,
                            eyebrow: unlocked.count <= 1 ? DiscoveryCopy.startHere : DiscoveryCopy.nextUp,
                            selected: selection == set, divider: !sectionEnds.contains(set),
                            live: live, fixedTime: fixedTime
                        ) { selection = set }
                            .id(set)
                    }
                }
            }
            .frame(width: listWidth, alignment: .leading)
            .padding(.top, 28)
            .padding(.bottom, 64)
            .frame(maxWidth: .infinity)
        }
        .scrollIndicators(.automatic)
        .scrollPosition($position)
        .onChange(of: settled) { _, now in
            if now { position.scrollTo(edge: .top) }
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel(DiscoveryCopy.ringLabel)
    }

    // MARK: order

    private enum Item: Hashable {
        case row(FeatureSet)
        case heading
        case more
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
        let rest = invitations.dropFirst()
        return invitations.prefix(1).map(Item.row)
            + (rest.isEmpty ? [] : [.more] + rest.map(Item.row))
            + (unlocked.isEmpty ? [] : [.heading] + unlocked.map(Item.row))
    }

    /// WHERE TO START: the most everyday set not yet unlocked.
    private var featured: FeatureSet? { invitations.first }

    /// The last row of each section: nothing follows it, so no hairline.
    private var sectionEnds: Set<FeatureSet> {
        Set([invitations.last, unlocked.last].compactMap { $0 })
    }

    private var complete: Bool { FeatureSet.allCases.allSatisfy { state($0) != .unseen } }

    // MARK: masthead

    /// The ring and one line. The header bar above already names the page.
    private var masthead: some View {
        HStack(alignment: .center, spacing: 12) {
            DiscoveryRing(
                progress: DiscoveryProgress(state, fresh: fresh), diameter: 26,
                breathing: live && fixedTime == nil)
                .frame(width: 30, height: 30)
            Text(complete ? DiscoveryCopy.completeHeadline : DiscoveryCopy.headline)
                .font(Typo.serif(21, weight: .medium))
                .foregroundStyle(Palette.ink)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    // MARK: section headings

    private func heading(_ title: String, top: CGFloat) -> some View {
        HStack(alignment: .center, spacing: 12) {
            Text(title)
                .textCase(.uppercase)
                .font(.system(size: 9.5, weight: .bold).width(.expanded))
                .tracking(0.7)
                .foregroundStyle(Palette.inkDim)
                .accessibilityAddTraits(.isHeader)
            Rectangle()
                .fill(Palette.hairline)
                .frame(height: 0.5)
        }
        .padding(.horizontal, DiscoveryRow.inset)
        // More air above than below: the heading belongs to what follows it.
        .padding(.top, top)
        .padding(.bottom, 6)
    }
}

// MARK: - a row

struct DiscoveryRow: View {
    let set: FeatureSet
    let state: SetState
    /// In the Unlocked section: the same row, tagged when there is news.
    let unlocked: Bool
    var fresh = false
    /// The page's one card: where to start.
    var featured = false
    var eyebrow = ""
    /// This row's guide is the one open.
    var selected = false
    /// A hairline under the row. Off for the last of a section.
    var divider = true
    var live = true
    var fixedTime: Double? = nil
    var open: () -> Void = {}

    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var hovering = false

    /// The row's wash runs this far past its content, so the content of every
    /// row, the masthead and the heading share one left edge.
    static let inset: CGFloat = 10
    private static let gap: CGFloat = 14
    private var picture: CGFloat { featured ? 190 : 64 }

    private var kept: Bool { state == .kept && unlocked }

    var body: some View {
        HStack(alignment: .center, spacing: featured ? 18 : Self.gap) {
            // ONLY UNDER THE POINTER. Everything else holds its poster.
            DiscoveryVignette(
                set: set, live: live && hovering, fixedTime: fixedTime, cornerRadius: featured ? Radius.concentric(Radius.pane, inset: 12) : 5)
                .frame(width: picture)

            if featured {
                VStack(alignment: .leading, spacing: 4) {
                    Text(eyebrow)
                        .font(.system(size: 9, weight: .bold).width(.expanded))
                        .tracking(0.6)
                        .foregroundStyle(Palette.accent)
                    Text(set.title)
                        .font(Typo.serif(21, weight: .medium))
                        .foregroundStyle(Palette.ink)
                    Text(set.line)
                        .font(.system(size: 12.5))
                        .foregroundStyle(Palette.inkDim)
                        .lineSpacing(1.5)
                        .fixedSize(horizontal: false, vertical: true)
                    HStack(spacing: 5) {
                        ForEach(set.keys, id: \.self) { DiscoveryKeycap($0) }
                    }
                    .padding(.top, 5)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            } else {
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    Text(set.title)
                        .font(Typo.serif(15.5, weight: .medium))
                        .foregroundStyle(Palette.ink)
                    tag
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }

            Image(systemName: "chevron.right")
                .font(.system(size: 10, weight: .semibold))
                .foregroundStyle(selected ? Palette.accent : Palette.inkFaintest)
                .opacity(hovering || selected ? 1 : 0)
        }
        .padding(.vertical, featured ? 12 : 6)
        .padding(.leading, featured ? 12 : Self.inset)
        .padding(.trailing, Self.inset)
        .background {
            RoundedRectangle(cornerRadius: featured ? Radius.pane : Radius.row, style: .continuous)
                .fill(fill)
        }
        .overlay {
            // The card's edge, even all round. Plain rows have none.
            RoundedRectangle(cornerRadius: Radius.pane, style: .continuous)
                .strokeBorder(selected ? Palette.accent.opacity(0.5) : Palette.hairline, lineWidth: 0.75)
                .opacity(featured ? 1 : 0)
        }
        .overlay(alignment: .bottom) {
            Rectangle().fill(Palette.hairline).frame(height: 0.5)
                .padding(.leading, Self.inset + picture + Self.gap)
                .padding(.trailing, Self.inset)
                .opacity(divider && !featured && !selected && !hovering ? 1 : 0)
        }
        .contentShape(Rectangle())
        .onTapGesture(perform: open)
        .onHover { hovering = $0 }
        .animation(reduceMotion ? nil : .smooth(duration: 0.18), value: hovering)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(
            "\(featured ? eyebrow.capitalized + ": " : "")\(set.title), \(set.stateWords(state)). \(set.line)")
        .accessibilityHint("Opens the guide.")
        .accessibilityAddTraits(selected ? [.isButton, .isSelected] : .isButton)
        .accessibilityAction(.default, open)
    }

    private var fill: Color {
        if selected { return Palette.accentSoft }
        if featured { return Palette.hairline.opacity(hovering ? 0.55 : 0.3) }
        return Palette.hairline.opacity(hovering ? 0.4 : 0)
    }

    /// Kept and just-unlocked say so in a word beside the name; plain unlocked
    /// and the invitations say nothing: the section already says what they
    /// are, and "not yet" is not a state worth a label.
    private var tag: some View {
        Text(fresh ? DiscoveryCopy.freshTag : DiscoveryCopy.keptTag)
            .font(.system(size: 8.5, weight: .bold).width(.expanded))
            .tracking(0.5)
            .foregroundStyle(fresh ? DiscoveryInk.ringHot : Palette.accent)
            .fixedSize()
            // Opacity, not an `if`: the tag comes and goes as the row settles.
            .opacity(unlocked && (fresh || kept) ? 1 : 0)
    }
}

// MARK: - the guide

/// ONE SET, TAUGHT: the picture large and playing (it is the one thing on the
/// page being looked at), then the gesture as a short run of steps, each with
/// the key that does it. Words from `DiscoveryCopy`, nothing invented here.
struct DiscoveryGuide: View {
    let set: FeatureSet
    let state: SetState
    var live = true
    var fixedTime: Double? = nil

    var body: some View {
        ScrollView(.vertical) {
            VStack(alignment: .leading, spacing: 0) {
                DiscoveryVignette(set: set, live: live, fixedTime: fixedTime, cornerRadius: 12)
                    .frame(maxWidth: 440)
                    .padding(.bottom, 20)

                Text(DiscoveryCopy.guideEyebrow(state))
                    .font(.system(size: 9.5, weight: .bold).width(.expanded))
                    .tracking(0.6)
                    .foregroundStyle(Palette.accent)
                    .padding(.bottom, 5)
                Text(set.title)
                    .font(Typo.serif(26, weight: .medium))
                    .foregroundStyle(Palette.ink)
                Text(set.line)
                    .font(.system(size: 13.5))
                    .foregroundStyle(Palette.inkDim)
                    .lineSpacing(2.5)
                    .fixedSize(horizontal: false, vertical: true)
                    .padding(.top, 5)

                Rectangle().fill(Palette.hairline).frame(height: 0.5)
                    .padding(.vertical, 18)

                VStack(alignment: .leading, spacing: 13) {
                    ForEach(Array(set.guide.enumerated()), id: \.offset) { _, step in
                        HStack(alignment: .firstTextBaseline, spacing: 12) {
                            // A fixed gutter, so the words line up whether a
                            // step has a key or is just something that happens.
                            Group {
                                if let key = step.key {
                                    DiscoveryKeycap(key)
                                } else {
                                    Circle().fill(Palette.inkFaintest).frame(width: 4, height: 4)
                                        .alignmentGuide(.firstTextBaseline) { $0[.bottom] + 4 }
                                }
                            }
                            .frame(width: 64, alignment: .trailing)
                            Text(step.text)
                                .font(.system(size: 13.5))
                                .foregroundStyle(Palette.ink)
                                .lineSpacing(2.5)
                                .fixedSize(horizontal: false, vertical: true)
                        }
                    }
                }
            }
            .frame(maxWidth: 480, alignment: .leading)
            .padding(.horizontal, 32)
            .padding(.top, 28)
            .padding(.bottom, 56)
            .frame(maxWidth: .infinity)
        }
        .accessibilityElement(children: .contain)
    }
}

// MARK: - keycaps

/// The page's keycap: a touch more of a KEY than the app's inline `Kbd`, since
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
                RoundedRectangle(cornerRadius: Radius.chip, style: .continuous)
                    .fill(Palette.readerBackground.opacity(0.85))
                    .shadow(color: .black.opacity(0.14), radius: 0.5, y: 1)
            )
            .overlay(
                RoundedRectangle(cornerRadius: Radius.chip, style: .continuous)
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
            DiscoveryVignette(set: set, live: live, fixedTime: fixedTime, cornerRadius: Radius.concentric(Radius.pane, inset: 9))
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
        .passbandGlass(.pane, cornerRadius: Radius.pane, tint: Palette.glassTintStrong)
        .shadow(color: .black.opacity(0.26), radius: 22, y: 10)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("\(set.foundTitle) \(set.foundLine)")
    }
}
