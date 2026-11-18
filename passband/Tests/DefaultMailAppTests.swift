// The default-mail-app ask, and every way it must NOT fire. The rule is pure
// and takes its inputs as arguments; Launch Services and UserDefaults stay
// out of a headless suite.

import Foundation

@main
@MainActor
struct DefaultMailAppTests {
    static var failures = 0
    static var checks = 0

    static func main() {
        onlyAfterASend()
        neverWhenAlreadyDefault()
        neverTwice()
        neverOnAnotherCard()
        whoIsTheDefault()

        if failures > 0 {
            print("FAILED: \(failures) of \(checks) checks")
            exit(1)
        }
        print("ok: \(checks) checks passed")
    }

    static func earned(
        sent: Bool = true, isDefault: Bool = false, asked: Bool = false, others: Bool = false
    ) -> Bool {
        DefaultMailApp.earned(
            sentFromPassband: sent, isDefault: isDefault, asked: asked, othersShowing: others)
    }

    static func onlyAfterASend() {
        expect(earned(), "a send, not the default, never asked: the ask")
        expect(!earned(sent: false), "no send, no ask: never at launch")
    }

    static func neverWhenAlreadyDefault() {
        expect(!earned(isDefault: true), "already the default is never asked")
    }

    static func neverTwice() {
        expect(!earned(asked: true), "asked once is asked forever")
        expect(!earned(isDefault: false, asked: true), "even when still not the default")
    }

    static func neverOnAnotherCard() {
        expect(!earned(others: true), "another one-time card up: not now")
        expect(earned(others: false), "and the next send gets the chance")
    }

    static func whoIsTheDefault() {
        let own = "app.passband.client"
        expect(
            DefaultMailApp.isPassband(handlerBundleId: own, ownBundleId: own),
            "the same bundle id is Passband")
        expect(
            DefaultMailApp.isPassband(handlerBundleId: "App.Passband.Client", ownBundleId: own),
            "bundle ids compare without case")
        expect(
            !DefaultMailApp.isPassband(handlerBundleId: "com.apple.mail", ownBundleId: own),
            "another app is not Passband")
        expect(
            !DefaultMailApp.isPassband(handlerBundleId: nil, ownBundleId: own),
            "no handler at all is not Passband")
        expect(
            !DefaultMailApp.isPassband(handlerBundleId: nil, ownBundleId: nil),
            "an unbundled build is never the default")
        expect(
            !DefaultMailApp.isPassband(handlerBundleId: "", ownBundleId: ""),
            "two empty ids are not a match")
    }

    static func expect(_ cond: Bool, _ what: String) {
        checks += 1
        if !cond {
            failures += 1
            print("  FAIL: \(what)")
        }
    }
}
