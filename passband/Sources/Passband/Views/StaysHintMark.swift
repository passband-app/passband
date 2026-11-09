import SwiftUI

extension View {
    /// The For your eyes row's half of the one-time "stays here" note
    /// (Model/StaysHint.swift): knows whether the row is in view, answers when
    /// the reader closes over it, and draws the ring and the callout while lit.
    func staysHint(threadId: String) -> some View {
        modifier(StaysHintMark(threadId: threadId))
    }
}

private struct StaysHintMark: ViewModifier {
    @Environment(AppStore.self) private var store
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    private var hint = StaysHint.shared
    let threadId: String
    @State private var inView = false

    init(threadId: String) { self.threadId = threadId }

    func body(content: Content) -> some View {
        let lit = hint.shownFor == threadId
        content
            // AN EVEN RING AND WASH around the whole row, never one lit edge.
            .background {
                RoundedRectangle(cornerRadius: Radius.well, style: .continuous)
                    .fill(Palette.accentSoft)
                    .overlay(
                        RoundedRectangle(cornerRadius: Radius.well, style: .continuous)
                            .strokeBorder(Palette.accent.opacity(0.55), lineWidth: 1.5))
                    .padding(-Space.s4)
                    .opacity(lit ? 1 : 0)
                    .allowsHitTesting(false)
            }
            .overlay(alignment: .bottomTrailing) {
                if lit {
                    callout
                        .alignmentGuide(.bottom) { $0[.top] - Space.s8 }
                        .transition(reduceMotion ? .opacity : .opacity.combined(with: .offset(y: -4)))
                }
            }
            .zIndex(lit ? 1 : 0)
            .animation(.easeInOut(duration: reduceMotion ? 0.3 : 0.45), value: lit)
            .onScrollVisibilityChange(threshold: 0.8) { inView = $0 }
            .onChange(of: store.threadId == nil) { _, closed in
                if closed { hint.rowReturned(threadId, visible: inView) }
            }
    }

    private var callout: some View {
        HStack(spacing: Space.s6) {
            Text("Stays here until you mark it done.")
            Kbd("e", size: 11)
        }
        .font(Typo.ui(12, weight: .medium))
        .foregroundStyle(Palette.ink)
        .fixedSize()
        .padding(.horizontal, ToastMetrics.hPadding)
        .padding(.vertical, Space.s8)
        .glassEffect(.regular.tint(Palette.glassTintStrong), in: .rect(cornerRadius: Radius.toast))
        .overlay(
            RoundedRectangle(cornerRadius: Radius.toast, style: .continuous)
                .strokeBorder(Palette.hairline, lineWidth: 1))
        .shadow(color: .black.opacity(0.18), radius: 12, y: 5)
        .allowsHitTesting(false)
        .accessibilityLabel("Stays here until you mark it done.")
    }
}
