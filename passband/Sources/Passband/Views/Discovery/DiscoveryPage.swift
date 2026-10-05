// THE DISCOVERY PAGE, the ring's one door on the rail: a table of contents,
// one line per feature set. `FeatureSet` case order is the column's order, the
// most everyday set first and each one more niche than the last.
//
// A LIST, NOT A SHELF. No cards, no borders, no glow, no keycaps: a small still
// of the gesture, the set's name and one line, hairlines between. The detail
// (the keys, the steps, the picture large and moving) is the guide's, one click
// away, so the list only has to be easy to run an eye down.
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
// A ROW IS A DOOR. Clicking one slides the column into the left half and opens
// that set's guide in the right half. THE COLUMN NEVER CHANGES WIDTH: it is the
// same measure centred on the page and in its half, so opening the guide moves
// the list without re-wrapping a word of it. Clicking the open row, the guide's
// close, or Esc puts the column back. In a window too narrow for both, the
// guide takes the whole page.
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
    /// The set whose guide is open, owned by the shell so Esc can close it.
    @Binding var selection: FeatureSet?

    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var width: CGFloat = 900
    /// HELD AT THE TOP THROUGH THE ARRIVAL. A scroll view keeps its place by
    /// anchoring to a row on screen, and the row on screen at arrival is the
    /// one that leaves: left alone, the page follows it to the bottom.
    @State private var position = ScrollPosition(edge: .top)

    /// The column's widest measure: a name and a line, read in one glance.
    static let column: CGFloat = 520
    private static let gutter: CGFloat = 24

    private var open: Bool { selection != nil }
    /// Room for the list AND the guide. Narrower, the guide stands alone.
    private var sideBySide: Bool { width >= 720 }
    /// ONE MEASURE, open or closed: what fits in half the page, so the list
    /// can slide into its half without a line of it re-wrapping.
    private var listWidth: CGFloat {
        let room = sideBySide ? width / 2 : width
        return max(0, min(Self.column, room - 2 * Self.gutter))
    }

    var body: some View {
        HStack(spacing: 0) {
            list
                // The container narrows; the column inside it does not.
                .frame(maxWidth: open ? (sideBySide ? width / 2 : 0) : .infinity)
                .clipped()
                .opacity(open && !sideBySide ? 0 : 1)
                .allowsHitTesting(!open || sideBySide)
            if let set = selection {
                Rectangle().fill(Palette.hairline).frame(width: 0.5)
                    .transition(.opacity)
                DiscoveryGuide(
                    set: set, state: state(set), live: live, fixedTime: fixedTime,
                    showsBack: !sideBySide
                ) { selection = nil }
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                    .transition(.move(edge: .trailing).combined(with: .opacity))
            }
        }
        .animation(reduceMotion ? nil : .smooth(duration: 0.32), value: open)
        .onGeometryChange(for: CGFloat.self) { $0.size.width } action: { width = $0 }
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
                        unlockedHeading
                            .transition(.opacity)
                    case .row(let set):
                        let placed = isPlacedUnlocked(set)
                        DiscoveryRow(
                            set: set, state: state(set),
                            unlocked: placed, fresh: placed && fresh.contains(set),
                            selected: selection == set, divider: !sectionEnds.contains(set),
                            live: live, fixedTime: fixedTime
                        ) { selection = selection == set ? nil : set }
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

    // MARK: the Unlocked heading

    private var unlockedHeading: some View {
        HStack(alignment: .center, spacing: 12) {
            Text(DiscoveryCopy.unlockedHeading)
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
        .padding(.top, complete ? 4 : 26)
        .padding(.bottom, 8)
    }
}

// MARK: - a row

struct DiscoveryRow: View {
    let set: FeatureSet
    let state: SetState
    /// In the Unlocked section: the same row, tagged when there is news.
    let unlocked: Bool
    var fresh = false
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
    private static let picture: CGFloat = 92
    private static let gap: CGFloat = 14

    private var kept: Bool { state == .kept && unlocked }

    var body: some View {
        HStack(alignment: .center, spacing: Self.gap) {
            // ONLY UNDER THE POINTER. Everything else holds its poster.
            DiscoveryVignette(set: set, live: live && hovering, fixedTime: fixedTime, cornerRadius: 6)
                .frame(width: Self.picture)

            VStack(alignment: .leading, spacing: 2) {
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    Text(set.title)
                        .font(Typo.serif(16.5, weight: .medium))
                        .foregroundStyle(Palette.ink)
                    tag
                }
                Text(set.line)
                    .font(.system(size: 12))
                    .foregroundStyle(Palette.inkDim)
                    .lineSpacing(1.5)
                    .lineLimit(2)
                    .fixedSize(horizontal: false, vertical: true)
            }
            .frame(maxWidth: .infinity, alignment: .leading)

            Image(systemName: "chevron.right")
                .font(.system(size: 10, weight: .semibold))
                .foregroundStyle(selected ? Palette.accent : Palette.inkFaintest)
                .opacity(hovering || selected ? 1 : 0)
        }
        .padding(.vertical, 9)
        .padding(.horizontal, Self.inset)
        .background {
            RoundedRectangle(cornerRadius: 10, style: .continuous)
                .fill(selected ? Palette.accentSoft : Palette.hairline.opacity(hovering ? 0.4 : 0))
        }
        .overlay(alignment: .bottom) {
            Rectangle().fill(Palette.hairline).frame(height: 0.5)
                .padding(.leading, Self.inset + Self.picture + Self.gap)
                .padding(.trailing, Self.inset)
                .opacity(divider && !selected && !hovering ? 1 : 0)
        }
        .contentShape(Rectangle())
        .onTapGesture(perform: open)
        .onHover { hovering = $0 }
        .animation(reduceMotion ? nil : .smooth(duration: 0.18), value: hovering)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("\(set.title), \(set.stateWords(state)). \(set.line)")
        .accessibilityHint("Opens the guide.")
        .accessibilityAddTraits(selected ? [.isButton, .isSelected] : .isButton)
        .accessibilityAction(.default, open)
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
    /// The narrow window's form: the guide stands alone, so it needs a way back.
    var showsBack = false
    let close: () -> Void

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
        .overlay(alignment: showsBack ? .topLeading : .topTrailing) {
            Button(action: close) {
                Image(systemName: showsBack ? "chevron.left" : "xmark")
                    .font(.system(size: 11, weight: .semibold))
                    .foregroundStyle(Palette.inkDim)
                    .frame(width: 26, height: 26)
                    .background(Circle().fill(Palette.hairline.opacity(0.5)))
                    .contentShape(Circle())
            }
            .buttonStyle(.plain)
            .help("Back to the list")
            .accessibilityLabel("Back to the list")
            .padding(12)
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
