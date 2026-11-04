import SwiftUI

/// Where the reader's one-time hint is drawn (Model/ReaderHint.swift): a layer
/// over the reader that takes no clicks and no keys. Centred over the email
/// first, a little larger; then it travels to the bottom right corner and
/// settles as a compact chip, clear of the undo toast (bottom left) and above
/// flow mode's hint when that is up. Reduce Motion crossfades instead of
/// travelling.
struct ReaderHintLayer: View {
    @Environment(AppStore.self) private var store
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    private var hint = ReaderHint.shared
    private var discovery = Discovery.shared
    /// Extra lift in the corner, for flow mode's hint underneath.
    let cornerLift: CGFloat

    init(cornerLift: CGFloat) { self.cornerLift = cornerLift }

    /// The layer's own size, for the centred card's height: a third of the
    /// way down the READER, which lands on the body of short mail and long
    /// mail alike and well clear of the sender line.
    @State private var area: CGSize = .zero

    var body: some View {
        ZStack {
            if store.threadId != nil, let placement = hint.placement {
                let corner = placement == .corner
                card(compact: corner)
                    .padding(.bottom, corner ? ToastMetrics.edge + cornerLift : 0)
                    .padding(.trailing, corner ? Space.s24 : 0)
                    // Centred: horizontally on the message column (the reader
                    // centres its column in this same area), its middle a third
                    // of the way down.
                    .padding(.top, corner ? 0 : max(0, area.height / 3 - Self.centredHalfHeight))
                    .frame(
                        maxWidth: .infinity, maxHeight: .infinity,
                        alignment: corner ? .bottomTrailing : .top)
                    // Under Reduce Motion the two placements are two views, so
                    // the change is a crossfade and nothing travels.
                    .id(reduceMotion ? AnyHashable(placement) : AnyHashable(0))
                    .transition(.opacity)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .onGeometryChange(for: CGSize.self) { $0.size } action: { area = $0 }
        .animation(
            reduceMotion ? .easeInOut(duration: 0.3) : .spring(duration: 0.7, bounce: 0.12),
            value: hint.placement)
        .animation(.easeInOut(duration: 0.25), value: store.threadId == nil)
        .allowsHitTesting(false)
        .onChange(of: store.threadId, initial: true) { old, new in
            // From closed to open, not one email to the next.
            if new != nil, old == nil || old == new { hint.readerOpened() }
        }
        .onChange(of: discovery.ledger.moveCounts[.emailDone] ?? 0) { _, _ in
            hint.doneCountChanged()
        }
    }

    /// Roughly half the centred card's height, so its middle (not its top)
    /// sits on the third.
    private static var centredHalfHeight: CGFloat { Space.scaled(30) }

    /// Centred: larger, warmer, ink keycaps on the accent wash, a stronger
    /// glass. In the corner: the compact chip, the house keycaps, plain glass.
    /// The one view shrinks and cools between the two as it travels.
    private func card(compact: Bool) -> some View {
        HStack(spacing: compact ? Space.s6 : Space.s10) {
            Text("Press")
            key("e", compact: compact)
            Text("to mark done, or")
            key("esc", compact: compact)
            Text("to leave.")
        }
        .font(compact ? Typo.ui(12) : Typo.ui(17, weight: .medium))
        .foregroundStyle(compact ? Palette.inkDim : Palette.ink)
        .fixedSize()
        .padding(.horizontal, compact ? ToastMetrics.hPadding : Space.s24)
        .padding(.vertical, compact ? ToastMetrics.vPadding : Space.s20)
        .glassEffect(
            compact ? .regular : .regular.tint(Palette.glassTintStrong),
            in: .rect(cornerRadius: Radius.toast))
        .overlay(
            RoundedRectangle(cornerRadius: Radius.toast, style: .continuous)
                .strokeBorder(Palette.hairline, lineWidth: 1)
        )
        .shadow(color: .black.opacity(compact ? 0.12 : 0.26), radius: compact ? 8 : 22, y: compact ? 3 : 10)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("Press E to mark done, or Escape to leave.")
    }

    @ViewBuilder
    private func key(_ label: String, compact: Bool) -> some View {
        if compact {
            Kbd(label, size: 11)
        } else {
            Text(label)
                .font(Typo.mono(15, weight: .semibold))
                .foregroundStyle(Palette.ink)
                .padding(.horizontal, Space.s8)
                .padding(.vertical, Space.s4)
                .background(
                    RoundedRectangle(cornerRadius: 6, style: .continuous)
                        .fill(Palette.accentSoft))
                .overlay(
                    RoundedRectangle(cornerRadius: 6, style: .continuous)
                        .strokeBorder(Palette.accent.opacity(0.35), lineWidth: 1))
                .fixedSize()
        }
    }
}
