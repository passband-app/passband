// THE DEFAULT MAIL APP ASK. One small card in the bottom-right corner, once,
// right after a send: should email links open in Passband? See DefaultMailApp
// for when it may go up and why only once.
//
// QUIET ON PURPOSE. Not the ad family's hero card and not a system alert: a
// line, a sentence and two buttons on the same glass, in the same corner the
// other one-time cards use. Not modal, and no `.defaultAction`: an unbidden
// card does not get to own Return, and the composer that just closed may have
// a ⌘Enter still in somebody's fingers.
//
// "Make default" hands over to the system, which shows its own confirmation.
// "Not now" never asks again. Settings > Mail keeps the button.

#if os(macOS)
    import SwiftUI

    struct DefaultMailNudgeCard: View {
        private var model: DefaultMailApp { DefaultMailApp.shared }

        @Environment(\.accessibilityReduceMotion) private var reduceMotion
        @State private var arrived = false
        @State private var leaving = false

        var body: some View {
            if model.showingNudge {
                card
                    .adCardPlacement()
            }
        }

        private var card: some View {
            let shown = arrived && !leaving
            return HStack(alignment: .top, spacing: 12) {
                Image(systemName: "envelope")
                    .font(Typo.ui(14, weight: .medium, fixed: true))
                    .foregroundStyle(Palette.inkDim)
                    .frame(width: 20, height: 20)
                    .accessibilityHidden(true)

                VStack(alignment: .leading, spacing: 10) {
                    VStack(alignment: .leading, spacing: 3) {
                        Text("Open email links in Passband?")
                            .font(Typo.ui(13, weight: .semibold, fixed: true))
                            .foregroundStyle(Palette.ink)
                        Text("Clicking an email address on a web page or in another app will start a new message here.")
                            .font(Typo.ui(11, fixed: true))
                            .foregroundStyle(Palette.inkFaint)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                    HStack(spacing: 8) {
                        Button("Make default") { close(madeDefault: true) }
                            .buttonStyle(.glass)
                            .controlSize(.small)
                        Button("Not now") { close(madeDefault: false) }
                            .buttonStyle(.plain)
                            .font(Typo.ui(11, weight: .medium, fixed: true))
                            .foregroundStyle(Palette.inkFaint)
                            .padding(.horizontal, 6)
                    }
                }
                Spacer(minLength: 0)
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 14)
            .frame(width: 300)
            .passbandGlass(.pane, cornerRadius: Radius.modal, tint: Palette.glassTintStrong)
            .shadow(color: .black.opacity(0.22), radius: 18, y: 8)
            .offset(y: shown ? 0 : 16)
            .opacity(shown ? 1 : 0)
            .accessibilityElement(children: .contain)
            .accessibilityLabel("Open email links in Passband")
            .onAppear {
                withAnimation(reduceMotion ? .easeOut(duration: 0.2) : .spring(response: 0.5, dampingFraction: 0.85).delay(0.6)) {
                    arrived = true
                }
            }
        }

        private func close(madeDefault: Bool) {
            // A double click lands inside the fade: one close, one event.
            guard !leaving else { return }
            if madeDefault { model.makeDefault() }
            withAnimation(.easeIn(duration: 0.2)) { leaving = true }
            Analytics.capture("default_mail_nudge_closed", ["made_default": madeDefault])
            Task {
                try? await Task.sleep(for: .milliseconds(200))
                model.showingNudge = false
                arrived = false
                leaving = false
            }
        }
    }
#endif
