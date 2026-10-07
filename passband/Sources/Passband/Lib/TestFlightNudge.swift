// WHEN TO TELL A MAC USER THE IPHONE APP EXISTS. Once, after the Mac app has
// had a couple of days to prove itself, and never to somebody who already has
// the phone app.
//
// "Already has it" is the daemon's answer, not ours (`StoreStats.ios_connected`):
// a phone that ever paired with, or registered for push on, this mailbox. Nil
// (an old daemon, or stats not loaded yet) means "do not know" and never asks.
//
// "A couple of days" counts from the first launch `ShareNudge` stamps, the one
// first-launch record this install keeps. An install from before that stamp
// existed counts from the first launch of the build that added it, which is
// the conservative direction.
//
// ASKED ONCE, EVER, like the share ask, and for the same reason. The link
// stays in Settings for anyone who changes their mind.

import Foundation
import Observation

@MainActor
@Observable
final class TestFlightNudge {
    static let shared = TestFlightNudge()

    /// The public TestFlight invite for the iPhone app.
    nonisolated static let joinURL = URL(string: "https://testflight.apple.com/join/nt7BD5H1")!

    /// How long after the first launch the ask becomes possible.
    nonisolated static let askAfter: TimeInterval = 2 * 24 * 3600

    private nonisolated static let askedKey = "passband.testflight.asked"
    /// WHEN it asked, beside the bool, so the agent ad can keep its distance
    /// (`AgentNudge.quietAfterIPhoneAd`). An install asked before this stamp
    /// existed has the bool and no date, which reads as long ago.
    private nonisolated static let askedAtKey = "passband.testflight.askedAt"

    /// When this install was shown the card, if it ever was since the stamp.
    nonisolated static var askedAt: Date? {
        UserDefaults.standard.object(forKey: askedAtKey) as? Date
    }

    private let defaults = UserDefaults.standard

    /// Whether the modal is up right now. Owned here so the one place that
    /// decides "ask" is the one place that records "asked".
    var showingNudge = false

    /// `PASSBAND_FORCE_TESTFLIGHT_NUDGE=1` puts the card up at launch without
    /// recording an ask, so it can be looked at without a two-day-old install
    /// and a daemon that has never seen a phone. Same posture as
    /// `PASSBAND_FORCE_TOUR`: a launch environment nobody sets by accident.
    nonisolated static let forced =
        ProcessInfo.processInfo.environment["PASSBAND_FORCE_TESTFLIGHT_NUDGE"] == "1"

    private init() {
        showingNudge = Self.forced
    }

    /// THE RULE, pure, so a headless suite can pin it. `iosConnected` nil is
    /// "the daemon has not said", which is a no: an ask that never comes beats
    /// one telling somebody about the app already on their phone.
    nonisolated static func earned(
        iosConnected: Bool?,
        asked: Bool,
        firstUse: Date?,
        now: Date
    ) -> Bool {
        guard iosConnected == false, !asked, let firstUse else { return false }
        return now.timeIntervalSince(firstUse) > askAfter
    }

    func shouldAsk(iosConnected: Bool?, now: Date = Date()) -> Bool {
        Self.earned(
            iosConnected: iosConnected,
            asked: defaults.bool(forKey: Self.askedKey),
            firstUse: defaults.object(forKey: ShareNudge.firstUseKey) as? Date,
            now: now)
    }

    /// Put the modal up and record that we did, before any answer: a crash
    /// with the card on screen has still asked.
    func ask() {
        defaults.set(true, forKey: Self.askedKey)
        defaults.set(Date(), forKey: Self.askedAtKey)
        showingNudge = true
    }

    /// Ask if this install has earned it. `othersShowing` is whether another
    /// one-time card is already on screen; two at once is a pile, and this one
    /// keeps until the next launch.
    func askIfEarned(iosConnected: Bool?, othersShowing: Bool) {
        guard !othersShowing, shouldAsk(iosConnected: iosConnected) else { return }
        ask()
    }
}
