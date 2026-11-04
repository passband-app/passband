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

    var body: some View {
        ZStack {
            if store.threadId != nil, let placement = hint.placement {
                let corner = placement == .corner
                card(compact: corner)
                    .padding(.bottom, corner ? ToastMetrics.edge + cornerLift : 0)
                    .padding(.trailing, corner ? Space.s24 : 0)
                    .frame(
                        maxWidth: .infinity, maxHeight: .infinity,
                        alignment: corner ? .bottomTrailing : .center)
                    // Under Reduce Motion the two placements are two views, so
                    // the change is a crossfade and nothing travels.
                    .id(reduceMotion ? AnyHashable(placement) : AnyHashable(0))
                    .transition(.opacity)
            }
        }
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

    private func card(compact: Bool) -> some View {
        let text = compact ? Typo.ui(12) : Typo.ui(15)
        let cap: CGFloat = compact ? 11 : 13
        return HStack(spacing: compact ? Space.s6 : Space.s8) {
            Text("Press")
            Kbd("e", size: cap)
            Text("to mark done, or")
            Kbd("esc", size: cap)
            Text("to leave.")
        }
        .font(text)
        .foregroundStyle(Palette.inkDim)
        .fixedSize()
        .padding(.horizontal, compact ? ToastMetrics.hPadding : Space.s20)
        .padding(.vertical, compact ? ToastMetrics.vPadding : Space.s16)
        .glassEffect(.regular, in: .rect(cornerRadius: Radius.toast))
        .overlay(
            RoundedRectangle(cornerRadius: Radius.toast, style: .continuous)
                .strokeBorder(Palette.hairline, lineWidth: 1)
        )
        .shadow(color: .black.opacity(compact ? 0.12 : 0.22), radius: compact ? 8 : 18, y: compact ? 3 : 8)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("Press E to mark done, or Escape to leave.")
    }
}
