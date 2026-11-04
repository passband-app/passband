import SwiftUI

/// THE CATCH-UP'S PROGRESS ROW: a bar of emails fetched against the total,
/// and one line under it. Drawn by the first-load veil and, once the person
/// has pressed Start now (or for any later catch-up), by the board's sync
/// banner, so the two read as the same thing in two places. The line is the
/// caller's: both build it from `AppStore.catchUpEstimate`.
struct CatchUpProgressRow: View {
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    let done: Int
    let total: Int
    let line: String
    var lineFont: Font = Typo.ui(13)

    var body: some View {
        VStack(alignment: .leading, spacing: Space.s8) {
            ProgressView(value: Double(min(done, max(total, 1))), total: Double(max(total, 1)))
                .progressViewStyle(.linear)
                .tint(Palette.accent)
                .animation(reduceMotion ? nil : .easeOut(duration: 0.6), value: done)
            Text(line)
                .font(lineFont)
                .foregroundStyle(Palette.inkDim)
                .monospacedDigit()
                .contentTransition(.numericText())
                .fixedSize(horizontal: false, vertical: true)
        }
        .accessibilityElement(children: .combine)
    }
}
