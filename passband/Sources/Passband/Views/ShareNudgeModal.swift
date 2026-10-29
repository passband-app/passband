// THE TWO-WEEK ASK. After a fortnight of actually using Passband, once and only
// once, a small card asks whether there is anyone worth sharing it with.
//
// MODAL, WITH A SCRIM, which is a heavier hand than `UpdateAlert`'s floating
// card and is meant to be: an update is something the app needs and can ask for
// again tomorrow, and this is a favour that gets exactly one chance. Small,
// centred, and either button ends it forever (see `ShareNudge`).
//
// NO `.defaultAction` ON THE PRIMARY BUTTON, and this is a rule with a scar
// behind it: `KeyDispatch`'s monitor only swallows Return on a surface that
// bound Enter, so a default action on an UNBIDDEN card turns whatever Return
// the user was about to press into a press of this one. A card nobody asked for
// does not get to own the Return key.

import SwiftUI

struct ShareNudgeModal: View {
    @Environment(AppStore.self) private var store
    private var nudge: ShareNudge { ShareNudge.shared }

    var body: some View {
        if nudge.showingNudge {
            // The scrim is tappable, and tapping it is "not now": a modal with
            // no way out but a button is a modal people learn to resent.
            OverlayScrim(onDismiss: dismiss) { card }
                .animation(Motion.toast, value: nudge.showingNudge)
        }
    }

    private var card: some View {
        ModalCard(width: 380, tint: Palette.glassTintStrong) {
            Text("You've been using Passband for 2 weeks!")
                .font(Typo.modalTitle)
                .foregroundStyle(Palette.ink)
                .fixedSize(horizontal: false, vertical: true)

            Text(
                "It would help us tremendously if you have any friends you think would also find "
                    + "it useful."
            )
            .font(Typo.rowSub)
            .foregroundStyle(Palette.inkDim)
            .fixedSize(horizontal: false, vertical: true)

            ModalButtons {
                Button("Not now") { dismiss() }
                    .buttonStyle(.glass)
                Button("Share Passband") { share() }
                    .buttonStyle(.glassProminent)
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Share Passband with friends")
    }

    private func dismiss() {
        nudge.showingNudge = false
        Analytics.capture("invite_nudge_dismissed")
    }

    private func share() {
        nudge.showingNudge = false
        store.openShareSheet(from: .nudge)
        Analytics.capture("invite_nudge_accepted")
    }
}
