import SwiftUI

/// THE FIRST LOOK at a newly connected mailbox (Model/FirstLoad.swift): one
/// card over the real board while the daemon works through the mail. The board
/// stays live and blurred underneath (`AppStore.modalOverlayOpen`), so what
/// this card counts is filling in behind it. It goes by itself when the
/// catch-up ends, or at "Start now", Enter or Esc.
struct FirstLoadVeil: View {
    @Environment(AppStore.self) private var store
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var appeared = false

    private var firstLoad: FirstLoad { store.firstLoad }

    var body: some View {
        OverlayScrim(onDismiss: { firstLoad.dismiss(store) }) {
            ModalCard(width: 640) {
                VStack(alignment: .leading, spacing: Space.scaled(22)) {
                    Label("PASSBAND", systemImage: "sparkles")
                        .font(Typo.sectionLabel)
                        .tracking(2)
                        .foregroundStyle(Palette.accentInk)
                    VStack(alignment: .leading, spacing: Space.s10) {
                        Text(firstLoad.finishing ? "Your inbox is ready." : "Working through your inbox.")
                            .font(Typo.serif(32, weight: .medium))
                            .foregroundStyle(Palette.ink)
                            .contentTransition(.opacity)
                        Text("Your board is filling in behind this. Start whenever you like.")
                            .font(Typo.ui(15))
                            .foregroundStyle(Palette.inkDim)
                    }
                    progress
                    if let shown = firstLoad.snapshot {
                        HStack(spacing: Space.s10) {
                            tile(shown.sorted, "emails sorted", "tray", Palette.accent)
                            tile(shown.noise, "set aside as noise", "moon", Palette.positive)
                            tile(shown.attention, "need your attention", "eye", Palette.accentInk)
                        }
                        .animation(reduceMotion ? nil : .easeOut(duration: 0.35), value: shown)
                    }
                    if let trouble {
                        Label(trouble, systemImage: store.daemonDown ? "wifi.exclamationmark" : "exclamationmark.circle")
                            .font(Typo.ui(13))
                            .foregroundStyle(Palette.inkDim)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                    HStack(spacing: Space.s14) {
                        Button("Start now") { firstLoad.dismiss(store) }
                            .buttonStyle(.borderedProminent)
                        KeyHint("enter", "start now")
                    }
                }
                .padding(Space.s14)
            }
            .opacity(appeared ? 1 : 0)
            .scaleEffect(appeared || reduceMotion ? 1 : 0.96)
            .offset(y: appeared || reduceMotion ? 0 : 12)
        }
        .onAppear {
            withAnimation(reduceMotion ? nil : .easeOut(duration: 0.45)) { appeared = true }
        }
        .keyContext(.modal)
        .keyBindings(.modal, [
            KeyBinding("Enter", "start now") { firstLoad.dismiss(store) },
            KeyBinding("Escape", "close") { firstLoad.dismiss(store) },
        ])
    }

    /// The bar, and under it how far along and roughly how long is left. The
    /// estimate is only ever the last poll's: it does not tick between them.
    @ViewBuilder
    private var progress: some View {
        let shown = firstLoad.snapshot
        VStack(alignment: .leading, spacing: Space.s8) {
            ProgressView(
                value: Double(shown?.done ?? 0),
                total: Double(max(shown?.total ?? 1, 1))
            )
            .progressViewStyle(.linear)
            .tint(Palette.accent)
            .animation(reduceMotion ? nil : .easeOut(duration: 0.6), value: shown?.done)
            Text(caption(shown))
                .font(Typo.ui(13))
                .foregroundStyle(Palette.inkDim)
                .monospacedDigit()
                .contentTransition(.numericText())
        }
        .accessibilityElement(children: .combine)
    }

    private func caption(_ shown: FirstLoad.Snapshot?) -> String {
        guard let shown else { return "Getting started…" }
        let count = "\(shown.done.formatted()) of \(shown.total.formatted()) emails"
        if firstLoad.finishing { return "\(count). All caught up." }
        // A daemon that is not answering has no pace to report.
        if trouble != nil { return count }
        return "\(count) · \(firstLoad.remainingPhrase)"
    }

    /// Why the bar may not move, when there is a reason. The old summary
    /// card's two degraded states, in its words.
    private var trouble: String? {
        if store.daemonDown { return "Connection interrupted. Your inbox will update when we reconnect." }
        if store.stats?.gmail?.connected == false {
            return "Your mail connection needs attention in Settings before new mail can arrive."
        }
        return nil
    }

    private func tile(_ count: Int, _ label: String, _ symbol: String, _ color: Color) -> some View {
        VStack(alignment: .leading, spacing: Space.s10) {
            Image(systemName: symbol).foregroundStyle(color)
            Text(count.formatted())
                .font(Typo.num(32, weight: .semibold))
                .contentTransition(.numericText())
            Text(label).font(Typo.rowSub).foregroundStyle(Palette.inkDim)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(Space.s14)
        .background(color.opacity(0.07), in: RoundedRectangle(cornerRadius: Radius.well, style: .continuous))
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("\(count) \(label)")
    }
}
