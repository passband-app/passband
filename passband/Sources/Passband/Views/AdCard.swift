// THE AD CARD'S MATERIALS, shared by the app's one-time ads (the iPhone ad,
// the agent-door ad). Lifted out of the iPhone ad as it was, so both are the
// same object in the same corner: the glass card, the live P7 phosphor hero
// fading into it, the pill naming what is new, the ✕.
//
// A FIXED COMPOSITION, so nothing here follows the density setting: every
// size is a literal on purpose (see TestFlightNudgeModal's header).
//
// Only one ad is ever up at a time (each one's `othersShowing`), which is why
// they can share `PhosphorScene.card`: its canvas follows the one view drawing
// it, and both draw it at the same size.

import SwiftUI

enum AdCard {
    static let width: CGFloat = 340
    static let heroHeight: CGFloat = 196
    static let cornerRadius: CGFloat = 20
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
    let pill: String
    let close: () -> Void
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
        .frame(height: AdCard.heroHeight)
        .clipShape(UnevenRoundedRectangle(
            topLeadingRadius: AdCard.cornerRadius, topTrailingRadius: AdCard.cornerRadius))
        .overlay(alignment: .topTrailing) { AdCloseButton(action: close) }
        .overlay(alignment: .topLeading) { AdPill(text: pill) }
    }
}

struct AdCloseButton: View {
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Image(systemName: "xmark")
                .font(.system(size: 10, weight: .bold))
                .foregroundStyle(.white.opacity(0.9))
                .frame(width: 24, height: 24)
                .background(.black.opacity(0.35), in: Circle())
                .overlay(Circle().strokeBorder(.white.opacity(0.18), lineWidth: 0.5))
                .contentShape(Circle())
        }
        .buttonStyle(.plain)
        .padding(10)
        .help("Close")
        .accessibilityLabel("Close")
    }
}

struct AdPill: View {
    let text: String

    var body: some View {
        Text(text)
            .font(.system(size: 9, weight: .bold).width(.expanded))
            .tracking(0.6)
            .foregroundStyle(.white.opacity(0.92))
            .padding(.horizontal, 9)
            .padding(.vertical, 5)
            .background(.black.opacity(0.32), in: Capsule())
            .overlay(Capsule().strokeBorder(Color(hex: 0x82BAF5).opacity(0.45), lineWidth: 0.5))
            .padding(12)
    }
}
