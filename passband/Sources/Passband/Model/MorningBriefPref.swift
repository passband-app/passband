// The app's half of the morning brief's switch (squelch-api's
// `morning_brief`): a push to the owner's iPhone once a local day near 8am
// saying what today holds. The daemon holds the switch, per mailbox, on by
// default; this reads and writes it for the Notifications card on both the
// Mac and the phone. The banner's words are the daemon's, and the phone's
// notification extension draws them (EventBanner.morningBriefBody).

import Foundation

extension AppStore {
    func refreshMorningBriefPref() async {
        let e = epoch
        do {
            let next = try await APIClient.shared.getMorningBriefSettings()
            guard e == epoch else { return }
            morningBriefPref = .loaded(next.enabled)
        } catch {
            guard e == epoch else { return }
            if (error as? APIError)?.kind == .notFound {
                morningBriefPref = .notOffered
            } else if case .loaded = morningBriefPref {
                // The stored switch did not move because the network blinked.
            } else {
                morningBriefPref = .unreadable
            }
        }
    }

    /// Throws so the card can say why; the switch only moves to what the
    /// daemon confirmed.
    func setMorningBrief(_ enabled: Bool) async throws {
        let e = epoch
        try await APIClient.shared.setMorningBriefSettings(enabled: enabled)
        guard e == epoch else { return }
        morningBriefPref = .loaded(enabled)
    }
}
