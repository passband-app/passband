import SwiftUI

// Mac only, like the walk hint it lands on (ThreadViewer.swift).
#if os(macOS)

/// Where flow mode's one-time explanation is drawn (Model/FlowHint.swift): a
/// layer over the reader that takes no clicks and no keys. Centred over the
/// email body a third of the way down, in the reader hint's warm style; then
/// it shrinks and travels into the bottom right, drawn there AS flow mode's
/// corner hint at that hint's exact inset, so when the real one takes over
/// nothing moves. Reduce Motion crossfades instead of travelling.
struct FlowHintLayer: View {
    @Environment(AppStore.self) private var store
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    private var hint = FlowHint.shared
    @State private var area: CGSize = .zero
    /// Whether s does anything on this walk (not on a walk of one), as the
    /// corner hint decides it.
    let canSkip: Bool

    init(canSkip: Bool) { self.canSkip = canSkip }

    var body: some View {
        ZStack {
            if store.threadId != nil, let placement = hint.placement {
                let landing = placement == .landing
                Group {
                    if landing {
                        WalkHint(canSkip: canSkip)
                    } else {
                        card
                    }
                }
                // The corner hint's own insets (RootView), so it lands on it.
                .padding(.bottom, landing ? Space.scaled(18) : 0)
                .padding(.trailing, landing ? Space.s24 : 0)
                .padding(.top, landing ? 0 : max(0, area.height / 3 - Space.scaled(44)))
                .frame(
                    maxWidth: .infinity, maxHeight: .infinity,
                    alignment: landing ? .bottomTrailing : .top)
                .id(reduceMotion ? AnyHashable(placement) : AnyHashable(0))
                .transition(.opacity)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .onGeometryChange(for: CGSize.self) { $0.size } action: { area = $0 }
        .animation(
            reduceMotion ? .easeInOut(duration: 0.3) : .spring(duration: 0.7, bounce: 0.1),
            value: hint.placement)
        .allowsHitTesting(false)
        .onChange(of: store.walk != nil) { was, now in
            if now, !was {
                hint.walkStarted { [store] in store.walk != nil && store.threadId != nil }
            } else if !now {
                hint.walkEnded()
            }
        }
    }

    private var card: some View {
        VStack(alignment: .leading, spacing: Space.s12) {
            Text("Flow mode. For your eyes, one email at a time.")
                .font(Typo.ui(17, weight: .medium))
                .foregroundStyle(Palette.ink)
            HStack(spacing: Space.s16) {
                pair("e", "done + next")
                if canSkip { pair("s", "save for later") }
                pair("esc", "leave")
            }
        }
        .fixedSize()
        .padding(.horizontal, Space.s24)
        .padding(.vertical, Space.s20)
        .glassEffect(.regular.tint(Palette.glassTintStrong), in: .rect(cornerRadius: Radius.toast))
        .overlay(
            RoundedRectangle(cornerRadius: Radius.toast, style: .continuous)
                .strokeBorder(Palette.hairline, lineWidth: 1)
        )
        .shadow(color: .black.opacity(0.26), radius: 22, y: 10)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(
            canSkip
                ? "Flow mode. For your eyes, one email at a time. E: done and next. S: save for later. Escape: leave."
                : "Flow mode. For your eyes, one email at a time. E: done and next. Escape: leave.")
    }

    private func pair(_ key: String, _ label: String) -> some View {
        HStack(spacing: Space.s8) {
            Text(key)
                .font(Typo.mono(15, weight: .semibold))
                .foregroundStyle(Palette.ink)
                .padding(.horizontal, Space.s8)
                .padding(.vertical, Space.s4)
                .background(RoundedRectangle(cornerRadius: 6, style: .continuous).fill(Palette.accentSoft))
                .overlay(
                    RoundedRectangle(cornerRadius: 6, style: .continuous)
                        .strokeBorder(Palette.accent.opacity(0.35), lineWidth: 1))
                .fixedSize()
            Text(label)
                .font(Typo.ui(14))
                .foregroundStyle(Palette.inkDim)
        }
    }
}
#endif
