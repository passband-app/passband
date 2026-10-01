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

        print("testflight-nudge: ok")
    }
}
