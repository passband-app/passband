// THE AD CARD'S MATERIALS, shared by the app's one-time ads (the iPhone ad,
// the agent-door ad). Lifted out of the iPhone ad as it was, so both are the
// same object in the same corner: the glass card, the live P7 phosphor hero
// fading into it, the pill naming what is new, the ✕.
//
// A FIXED COMPOSITION, so nothing inside the card follows the density setting:
// every size is a literal or an `AdCard` constant, and type is `fixed:`, on
// purpose (see TestFlightNudgeModal's header). Only the margin to the window
// corner (`adCardPlacement`) is room, and follows it.
//
// Only one ad is ever up at a time (each one's `othersShowing`), which is why
// they can share `PhosphorScene.card`: its canvas follows the one view drawing
// it, and both draw it at the same size.

import SwiftUI

enum AdCard {
    static let width: CGFloat = 340
    static let heroHeight: CGFloat = 196
    static let cornerRadius: CGFloat = Radius.modal
    /// THE HERO'S ONE INSET: the pill and the ✕ sit this far in from the
    /// card's top corners, so the two controls share an edge (the ✕ used to
    /// sit 2pt closer to the corner than the pill).
    static let heroInset: CGFloat = 12
    /// The words under the hero: one text edge for every ad.
    static let wordsHorizontal: CGFloat = 18
    static let wordsTop: CGFloat = 16
    static let wordsBottom: CGFloat = 18
    /// Between the words' blocks (headline, feature, call to action), and
    /// between a headline and its line. The agent ad runs tighter on purpose
    /// to hold its extra line in the same footprint.
    static let wordsGap: CGFloat = 14
    static let headlineGap: CGFloat = 6
}

extension View {
    /// The ad family's words padding, so every ad's copy starts on one edge.
    func adWordsPadding() -> some View {
        padding(.horizontal, AdCard.wordsHorizontal)
            .padding(.top, AdCard.wordsTop)
            .padding(.bottom, AdCard.wordsBottom)
    }

    /// The bottom-right corner an ad card is set in: the same margin the
    /// discovery moment keeps. The margin is ROOM, so it follows the density;
    /// the card inside it does not.
    func adCardPlacement() -> some View {
        padding(Space.s20)
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .bottomTrailing)
    }
}

/// The card itself: glass, its shadow, and the arrival from the corner.
struct AdCardChrome: ViewModifier {
    let arrived: Bool
    let leaving: Bool
    let label: String

    func body(content: Content) -> some View {
        let shown = arrived && !leaving
        content
            .frame(width: AdCard.width)
            .passbandGlass(.pane, cornerRadius: AdCard.cornerRadius, tint: Palette.glassTintStrong)
            .shadow(color: .black.opacity(0.35), radius: 28, y: 14)
            .offset(x: shown ? 0 : 40, y: shown ? 0 : 24)
            .opacity(shown ? 1 : 0)
            .scaleEffect(shown ? 1 : 0.96, anchor: .bottomTrailing)
            .accessibilityElement(children: .contain)
            .accessibilityLabel(label)
    }
}

extension View {
    func adCardChrome(arrived: Bool, leaving: Bool, label: String) -> some View {
        modifier(AdCardChrome(arrived: arrived, leaving: leaving, label: label))
    }
}

/// The hero: the live phosphor, its floor fading into the glass, and whatever
/// the ad shows rising out of the bottom edge. The pill top-left, the ✕
/// top-right.
struct AdHero<Content: View>: View {
    /// Whether the window is key. The phosphor pauses otherwise.
    let live: Bool
    /// Nil draws no pill (the veil's ad wears the wordmark there instead).
    let pill: String?
    /// Nil draws no ✕: the first-load veil's ad closes with the veil.
    let close: (() -> Void)?
    /// Grow past `heroHeight` to fill a taller card (the veil's pair, which
    /// shares one height); the phone stays on the floor.
    var stretch = false
    @ViewBuilder var content: Content

    var body: some View {
        ZStack(alignment: .bottom) {
            PhosphorBackdrop(paused: !live, scene: .card)
                .allowsHitTesting(false)
                .accessibilityHidden(true)

            // The phosphor's floor bleeds into the card rather than ending at
            // a seam: the hero fades into the glass under it.
            LinearGradient(
                colors: [.clear, .black.opacity(0.35)],
                startPoint: .center, endPoint: .bottom
            )
            .allowsHitTesting(false)

            content
        }
        .frame(minHeight: AdCard.heroHeight, maxHeight: stretch ? .infinity : AdCard.heroHeight)
        .clipShape(UnevenRoundedRectangle(
            topLeadingRadius: AdCard.cornerRadius, topTrailingRadius: AdCard.cornerRadius))
        .overlay(alignment: .topTrailing) {
            if let close { AdCloseButton(action: close) }
        }
        .overlay(alignment: .topLeading) {
            if let pill { AdPill(text: pill) }
        }
    }
}

struct AdCloseButton: View {
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Image(systemName: "xmark")
                .font(Typo.ui(10, weight: .bold, fixed: true))
                .foregroundStyle(.white.opacity(0.9))
                .frame(width: 24, height: 24)
                .background(.black.opacity(0.35), in: Circle())
                .overlay(Circle().strokeBorder(.white.opacity(0.18), lineWidth: 0.5))
                .contentShape(Circle())
        }
        .buttonStyle(.plain)
        .padding(AdCard.heroInset)
        .help("Close")
        .accessibilityLabel("Close")
    }
}

/// The ad's pill: the shared `HeroPill` (What's New wears it too), at the
/// composition's fixed size and on the hero's one inset.
struct AdPill: View {
    let text: String

    var body: some View {
        HeroPill(text: text, fixed: true)
            .padding(AdCard.heroInset)
    }
}
