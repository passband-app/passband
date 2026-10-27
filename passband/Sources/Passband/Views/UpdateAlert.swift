// The new-version card. Floats over whatever page is on screen, bottom centre,
// and says one thing: there is a newer Passband, press this to be running it.
//
// Bottom CENTRE is the only free corner: the toast stack owns bottom-left
// (ActionLayer, inset 74 for the rail), Settings stamps its version number
// bottom-right, and the top edge belongs to the top bar and the connection
// banner. Not modal, because a mail app is a thing you are reading and an
// update is never urgent enough to take the window; not a toast either,
// because a toast leaves on a timer and this waits to be answered.

import SwiftUI

struct UpdateAlert: View {
    @ObservedObject private var updater = Updater.shared

    /// ONE card across every phase. The offer and the narration that follows
    /// it share a corner and an inset, so pressing Update changes the words in
    /// the card rather than swapping it for a smaller one mid-download.
    private static var hPadding: CGFloat { Space.scaled(15) }
    private static var vPadding: CGFloat { Space.s12 }

    var body: some View {
        Group {
            switch updater.phase {
            case .idle:
                EmptyView()
            case .checking:
                narration(text: "checking for updates", fraction: nil, spinner: true)
            case .available(let version):
                offer(
                    title: "Passband \(version) is available",
                    // What the button DOES, said before it is pressed: the app
                    // quits and comes back, and anyone typing an email deserves
                    // to know that before they find out.
                    detail: "Updating installs it and restarts Passband.",
                    action: "Update", perform: updater.installAndRelaunch)
            case .ready(let version):
                offer(
                    title: "Passband \(version) is ready",
                    detail: "It installs when you quit, or restart now.",
                    action: "Restart", perform: updater.restartNow)
            case .downloading(let fraction):
                narration(text: "downloading Passband", fraction: fraction, spinner: fraction == nil)
            case .extracting(let fraction):
                narration(text: "unpacking the update", fraction: fraction, spinner: false)
            case .installing:
                narration(text: "restarting into the new version", fraction: nil, spinner: true)
            }
        }
        .animation(Motion.toast, value: updater.phase)
    }

    // MARK: - the offer

    private func offer(
        title: String, detail: String, action: String, perform: @escaping () -> Void
    ) -> some View {
        HStack(spacing: Space.scaled(13)) {
            Image(systemName: "arrow.down.circle.fill")
                .font(Typo.ui(19, weight: .medium))
                .foregroundStyle(Palette.accent)

            VStack(alignment: .leading, spacing: Space.s2) {
                Text(title)
                    .font(Typo.ui(13, weight: .semibold))
                    .foregroundStyle(Palette.ink)
                Text(detail)
                    .font(Typo.rowSub)
                    .foregroundStyle(Palette.inkFaint)
            }

            Spacer(minLength: Space.s10)

            Button("Later") { updater.later() }
                .buttonStyle(.glass)
                .font(Typo.ui(12, weight: .medium))

            // No .defaultAction: the card arrives unbidden, and on any page
            // without its own Return binding that made Return a relaunch.
            Button(action, action: perform)
                .buttonStyle(.glassProminent)
                .font(Typo.ui(12, weight: .semibold))
        }
        .padding(.horizontal, Self.hPadding)
        .padding(.vertical, Self.vPadding)
        .frame(maxWidth: 520)
        .passbandGlass(.pane, cornerRadius: Radius.toast, tint: Palette.glassTintStrong)
        .transition(.move(edge: .bottom).combined(with: .opacity))
        .accessibilityElement(children: .contain)
        .accessibilityLabel(title)
    }

    // MARK: - the work

    /// Everything after the button: one line and, when there is an honest
    /// fraction to draw, a bar. No cancel — Sparkle's own cancellation stops
    /// only the download, and a card offering to abandon a half-installed app
    /// would be offering something it cannot do.
    private func narration(text: String, fraction: Double?, spinner: Bool) -> some View {
        HStack(spacing: Space.scaled(11)) {
            if spinner {
                ProgressView()
                    .controlSize(.small)
                    .scaleEffect(0.8)
            } else {
                Image(systemName: "arrow.down.circle")
                    .font(Typo.ui(15, weight: .medium))
                    .foregroundStyle(Palette.accent)
            }

            Text(text)
                .font(Typo.rowSub)
                .foregroundStyle(Palette.inkDim)

            if let fraction {
                ProgressView(value: fraction)
                    .progressViewStyle(.linear)
                    .tint(Palette.accent)
                    .frame(width: 110)
                Text("\(Int(fraction * 100))%")
                    .font(Typo.num(11))
                    .foregroundStyle(Palette.inkFaint)
                    .monospacedDigit()
                    .frame(width: Space.scaled(32), alignment: .trailing)
            }
        }
        .padding(.horizontal, Self.hPadding)
        .padding(.vertical, Self.vPadding)
        .passbandGlass(.chrome, cornerRadius: Radius.toast, tint: Palette.glassTint)
        .transition(.move(edge: .bottom).combined(with: .opacity))
        .accessibilityElement(children: .combine)
        .accessibilityLabel(text)
    }
}
