// The iPhone ask, and every way it must NOT fire. The rule is a pure static,
// so this needs no daemon, clock, or UserDefaults.

import Foundation

@main
@MainActor
struct TestFlightNudgeTests {
    static let now = Date(timeIntervalSince1970: 1_755_600_000)
    static func ago(hours: Double) -> Date { now.addingTimeInterval(-hours * 3600) }

    static func earned(_ ios: Bool?, asked: Bool = false, firstUse: Date?) -> Bool {
        TestFlightNudge.earned(iosConnected: ios, asked: asked, firstUse: firstUse, now: now)
    }

    static func main() {
        // Three days in, no phone ever connected, never asked: ask.
        precondition(earned(false, firstUse: ago(hours: 72)))

        // "More than two days": 47h and exactly 48h are not, 49h is.
        precondition(!earned(false, firstUse: ago(hours: 47)))
        precondition(!earned(false, firstUse: ago(hours: 48)))
        precondition(earned(false, firstUse: ago(hours: 49)))

        // Somebody with the phone app is never told about it.
        precondition(!earned(true, firstUse: ago(hours: 500)))
        // A daemon that has not answered (or is too old to) is not a "no phone".
        precondition(!earned(nil, firstUse: ago(hours: 500)))
        // Once, ever.
        precondition(!earned(false, asked: true, firstUse: ago(hours: 500)))
        // No first-launch stamp reads as "not yet".
        precondition(!earned(false, firstUse: nil))

        onboarding()
        print("testflight-nudge: ok")
    }

    /// Day one on the Mac: the veil's row, its hand-off, and the fallback
    /// card, and that the three together ask at most once.
    static func onboarding() {
        typealias N = TestFlightNudge

        // THE VEIL'S ROW. No phone, or not known (a fresh poll's nil is a
        // daemon too old to say): shown. A phone connected: never.
        precondition(N.offersInVeil(iosConnected: false, asked: false))
        precondition(N.offersInVeil(iosConnected: nil, asked: false))
        precondition(!N.offersInVeil(iosConnected: true, asked: false))
        // An install already asked (the two-day card, an earlier veil): not again.
        precondition(!N.offersInVeil(iosConnected: false, asked: true))

        // THE HAND-OFF. The row was up and the veil closed by itself, phone
        // still a no: the corner card carries the code on.
        precondition(N.handsOff(offered: true, early: false, iosConnected: false))
        // "Start now" (or Esc, or the scrim): they chose the board; not chased.
        precondition(!N.handsOff(offered: true, early: true, iosConnected: false))
        // The phone connected while the veil was up: nothing to hand off.
        precondition(!N.handsOff(offered: true, early: false, iosConnected: true))
        // The corner card needs a definite no, as it always has.
        precondition(!N.handsOff(offered: true, early: false, iosConnected: nil))
        // No row, nothing to hand off.
        precondition(!N.handsOff(offered: false, early: false, iosConnected: false))

        // THE FALLBACK. No veil, no phone, never asked: the card, shortly.
        precondition(N.earnedAfterOnboarding(veilShown: false, iosConnected: false, asked: false))
        // The veil had the row, so the fallback never doubles it.
        precondition(!N.earnedAfterOnboarding(veilShown: true, iosConnected: false, asked: false))
        precondition(!N.earnedAfterOnboarding(veilShown: false, iosConnected: true, asked: false))
        precondition(!N.earnedAfterOnboarding(veilShown: false, iosConnected: nil, asked: false))
        precondition(!N.earnedAfterOnboarding(veilShown: false, iosConnected: false, asked: true))

        // NEVER TWICE. Showing the row stamps the ask (`veilShown`), and once
        // stamped neither another veil, the fallback, nor the two-day card
        // asks again, however late.
        precondition(!N.offersInVeil(iosConnected: false, asked: true))
        precondition(!N.earnedAfterOnboarding(veilShown: false, iosConnected: false, asked: true))
        precondition(!earned(false, asked: true, firstUse: ago(hours: 500)))
    }
}
