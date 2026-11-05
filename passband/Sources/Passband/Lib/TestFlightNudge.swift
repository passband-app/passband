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
//
// OR ON DAY ONE, IN ONBOARDING (Mac). The phone is where the pushes that bring
// somebody back land, so the ask comes while they wait, not two days later:
//
//   * IN THE FIRST-LOAD VEIL, a QR row under the progress (`offersInVeil`).
//     They are sitting there anyway, so it costs no step and no click. Shown
//     on `iosConnected` nil as well as false: the veil only goes up on a
//     fresh poll, so nil there is a daemon too old to say, and a quiet row on
//     a waiting screen is cheap to be wrong about. Showing it is the ask, and
//     is stamped as one, so the two-day card never asks again.
//   * HANDED OFF when the veil closes by itself (`handsOff`): the catch-up can
//     end mid-scan, and the QR must not vanish from under a camera, so the
//     corner card carries it on, once. Not after "Start now": they chose the
//     board, and are not chased with a second card.
//   * THE CORNER CARD SHORTLY AFTER ONBOARDING when there was no veil to put
//     it in (`earnedAfterOnboarding`): a mailbox already caught up skips the
//     veil, and its owner is just as new.
//
// The corner card, whichever road reaches it, keeps the original rule's nil:
// it interrupts, so it needs a definite "no phone". Never a tour, never a
// lesson: an offer on a waiting screen.

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

    /// Where the card on screen came from, for `ios_nudge_closed`'s `source`:
    /// "nudge" (two days in), "first_load" (handed off by the veil) or
    /// "onboarding" (no veil, shortly after the first look).
    var source = "nudge"

    /// The veil is showing the iPhone row this session. Latched when the veil
    /// goes up, so the row does not come and go under the person's camera.
    var offeredInVeil = false
    /// "Copy link" was pressed on the veil's row, for `first_load_ended`.
    var copiedInVeil = false

    /// `PASSBAND_FORCE_TESTFLIGHT_NUDGE=1` puts the card up at launch without
    /// recording an ask, so it can be looked at without a two-day-old install
    /// and a daemon that has never seen a phone. Same posture as
    /// `PASSBAND_FIRST_LOAD`: a launch environment nobody sets by accident.
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

    /// THE VEIL'S RULE. Never to a mailbox a phone has connected to, and not
    /// to an install already asked. Nil shows (see the header).
    nonisolated static func offersInVeil(iosConnected: Bool?, asked: Bool) -> Bool {
        iosConnected != true && !asked
    }

    /// THE HAND-OFF. The veil showed the row and closed by itself (`early`
    /// false), and the phone is still a definite no: the corner card, now.
    nonisolated static func handsOff(offered: Bool, early: Bool, iosConnected: Bool?) -> Bool {
        offered && !early && iosConnected == false
    }

    /// THE FALLBACK. A first look that never raised the veil, by a mailbox no
    /// phone has seen, on an install never asked: the corner card, shortly.
    nonisolated static func earnedAfterOnboarding(
        veilShown: Bool, iosConnected: Bool?, asked: Bool
    ) -> Bool {
        !veilShown && !asked && iosConnected == false
    }

    /// How long after the first look ends, with no veil, before the card.
    nonisolated static let afterOnboardingDelay: Duration = .seconds(20)

    var asked: Bool { defaults.bool(forKey: Self.askedKey) }

    func shouldAsk(iosConnected: Bool?, now: Date = Date()) -> Bool {
        Self.earned(
            iosConnected: iosConnected,
            asked: defaults.bool(forKey: Self.askedKey),
            firstUse: defaults.object(forKey: ShareNudge.firstUseKey) as? Date,
            now: now)
    }

    /// Put the modal up and record that we did, before any answer: a crash
    /// with the card on screen has still asked.
    func ask(source: String = "nudge") {
        stampAsked()
        self.source = source
        showingNudge = true
    }

    private func stampAsked() {
        defaults.set(true, forKey: Self.askedKey)
        defaults.set(Date(), forKey: Self.askedAtKey)
    }

    /// The first-load veil went up. Decides, once, whether it carries the
    /// iPhone row, and stamps the ask if it does. `forced` (the veil's preview)
    /// shows it and stamps nothing.
    func veilShown(iosConnected: Bool?, forced: Bool) {
        // A veil dropped by a lost connection and raised again keeps its row:
        // the stamp from the first time must not take it away.
        guard !offeredInVeil else { return }
        if forced {
            offeredInVeil = iosConnected != true
            return
        }
        guard Self.offersInVeil(iosConnected: iosConnected, asked: asked) else { return }
        stampAsked()
        offeredInVeil = true
    }

    /// The veil came down. Hands off to the corner card when the rule says so;
    /// either way the row is done for the session.
    func veilEnded(early: Bool, iosConnected: Bool?) {
        let handOff = Self.handsOff(offered: offeredInVeil, early: early, iosConnected: iosConnected)
        offeredInVeil = false
        guard handOff, !showingNudge else { return }
        // Already stamped when the row went up.
        source = "first_load"
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
