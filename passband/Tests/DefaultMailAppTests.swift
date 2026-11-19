// The one ask to be the default mail app: the system's dialog alone, once
// ever, the first time an inbox loads over a verified connection, whether the
// install is new or from before the ask. The rule is pure; the model runs
// against a throwaway UserDefaults suite, a probe standing in for Launch
// Services and a request that records what was stamped when it came. The
// wiring a headless suite cannot run is pinned by reading the source, and so
// is the absence of the card this replaced.

import Foundation

@main
@MainActor
struct DefaultMailAppTests {
    static var failures = 0
    static var checks = 0

    static func main() {
        theRule()
        whoIsTheDefault()
        aNewInstallIsAskedOnce()
        anExistingInstallIsAskedOnceOnUpdate()
        theStampComesBeforeTheRequest()
        skippedWhenAlreadyTheDefault()
        blockedWaitsForTheNextLaunch()
        theOverride()
        theDelay()
        theWiringIsThere()
        noCardRemains()

        if failures > 0 {
            print("FAILED: \(failures) of \(checks) checks")
            exit(1)
        }
        print("ok: \(checks) checks passed")
    }

    static func rule(
        verified: Bool = true, blocked: Bool = false, isDefault: Bool = false,
        asked: Bool = false, forced: Bool = false
    ) -> Bool {
        DefaultMailApp.shouldAsk(
            verified: verified, blocked: blocked, isDefault: isDefault, asked: asked, forced: forced)
    }

    static func theRule() {
        expect(rule(), "verified, nothing in the way, not the default, never asked: asks")
        expect(!rule(verified: false), "not before the connection is verified")
        expect(!rule(blocked: true), "not over another one-time card")
        expect(!rule(isDefault: true), "not when already the default")
        expect(!rule(asked: true), "asked once is asked forever")
        expect(rule(asked: true, forced: true), "the override asks past the stamp")
        expect(!rule(isDefault: true, forced: true), "but not when already the default")
    }

    static func whoIsTheDefault() {
        let own = "app.passband.client"
        expect(DefaultMailApp.isPassband(handlerBundleId: own, ownBundleId: own), "same id")
        expect(
            DefaultMailApp.isPassband(handlerBundleId: "App.Passband.Client", ownBundleId: own),
            "ids compare without case")
        expect(
            !DefaultMailApp.isPassband(handlerBundleId: "com.apple.mail", ownBundleId: own),
            "another app is not Passband")
        expect(!DefaultMailApp.isPassband(handlerBundleId: nil, ownBundleId: own), "no handler")
        expect(!DefaultMailApp.isPassband(handlerBundleId: nil, ownBundleId: nil), "no bundle")
        expect(!DefaultMailApp.isPassband(handlerBundleId: "", ownBundleId: ""), "empty ids")
    }

    // MARK: - the model

    static func fresh() -> UserDefaults {
        let name = "DefaultMailAppTests.\(UUID().uuidString)"
        let d = UserDefaults(suiteName: name)!
        d.removePersistentDomain(forName: name)
        return d
    }

    /// One install: its defaults, a Launch Services answer, and a count of
    /// requests with whether the stamp was down at each.
    @MainActor
    final class Install {
        let defaults: UserDefaults
        var isDefault = false
        var requests = 0
        var stampedAtRequest: [Bool] = []

        init(_ defaults: UserDefaults = DefaultMailAppTests.fresh()) { self.defaults = defaults }

        /// A launch: a new model over the same defaults, as the app makes one.
        @MainActor func launch(forced: Bool = false) -> DefaultMailApp {
            DefaultMailApp(
                defaults: defaults, forced: forced, probe: { [unowned self] in self.isDefault },
                request: { [unowned self] in
                    self.requests += 1
                    self.stampedAtRequest.append(self.defaults.bool(forKey: DefaultMailApp.askedKey))
                })
        }
    }

    @discardableResult
    static func inboxLoaded(
        _ m: DefaultMailApp, verified: Bool = true, blocked: Bool = false
    ) -> Bool {
        var asked = false
        m.askIfNeverAsked(after: nil, conditions: { (verified, blocked) }, done: { asked = $0 })
        return asked
    }

    static func aNewInstallIsAskedOnce() {
        let i = Install()
        let m = i.launch()
        expect(!inboxLoaded(m, verified: false), "a new install waits for verification")
        expect(inboxLoaded(m), "its first verified inbox asks")
        expect(i.requests == 1, "one request")
        expect(!inboxLoaded(m), "the same launch never asks twice")
        expect(!inboxLoaded(i.launch()), "nor does the next launch")
        expect(i.requests == 1, "still one request, ever")
    }

    static func anExistingInstallIsAskedOnceOnUpdate() {
        // An install from before the ask has its own defaults, and no stamp:
        // the first launch of this version is its first chance.
        let i = Install()
        i.defaults.set(true, forKey: "passband.pref.tourCompleted")  // onboarded long ago
        expect(inboxLoaded(i.launch()), "the update's first verified inbox asks")
        expect(i.requests == 1, "once")
        expect(!inboxLoaded(i.launch()), "and never on a later launch")
        expect(i.requests == 1, "still once")
    }

    static func theStampComesBeforeTheRequest() {
        let i = Install()
        inboxLoaded(i.launch())
        expect(i.stampedAtRequest == [true], "the stamp was down when the request went")
        // A refusal or a crash after the request changes nothing: still asked.
        expect(i.defaults.bool(forKey: DefaultMailApp.askedKey), "stamped for good")
    }

    static func skippedWhenAlreadyTheDefault() {
        let i = Install()
        i.isDefault = true
        expect(!inboxLoaded(i.launch()), "already the default: no ask")
        expect(i.requests == 0, "no request")
        expect(!i.defaults.bool(forKey: DefaultMailApp.askedKey), "and no stamp")
        // If another app takes it back later, the never-asked install is asked.
        i.isDefault = false
        expect(inboxLoaded(i.launch()), "no longer the default, never asked: asks")
    }

    static func blockedWaitsForTheNextLaunch() {
        let i = Install()
        let m = i.launch()
        expect(!inboxLoaded(m, blocked: true), "another card up: no ask")
        expect(i.requests == 0 && !i.defaults.bool(forKey: DefaultMailApp.askedKey),
            "nothing requested, nothing stamped")
        expect(inboxLoaded(i.launch()), "the next launch asks")
        expect(i.requests == 1, "once")
    }

    static func theOverride() {
        let i = Install()
        i.defaults.set(true, forKey: DefaultMailApp.askedKey)
        expect(inboxLoaded(i.launch(forced: true)), "forced asks on an asked install")
        expect(i.requests == 1, "forced requests")
        let j = Install()
        inboxLoaded(j.launch(forced: true))
        expect(!j.defaults.bool(forKey: DefaultMailApp.askedKey), "forced writes no stamp")
        j.isDefault = true
        expect(!inboxLoaded(j.launch(forced: true)), "forced still skips the default")
    }

    static func theDelay() {
        expect(DefaultMailApp.askDelay == .seconds(1), "about a second after the inbox loads")
    }

    // MARK: - the source

    static func read(_ path: String) -> String {
        guard let s = try? String(contentsOfFile: path, encoding: .utf8) else {
            expect(false, "read \(path) (run from passband/)")
            return ""
        }
        return s
    }

    /// From `signature` to the next member declaration at the class's indent.
    static func body(of signature: String, in source: String) -> String {
        guard let start = source.range(of: signature) else { return "" }
        let rest = source[start.upperBound...]
        let ends = ["\n    func ", "\n    private func ", "\n    var ", "\n    // MARK:",
                    "\n        func ", "\n    #endif"]
            .compactMap { rest.range(of: $0)?.lowerBound }
        return String(rest[..<(ends.min() ?? rest.endIndex)])
    }

    static func theWiringIsThere() {
        let model = read("Sources/Passband/Lib/DefaultMailApp.swift")
        let decide = body(of: "private func decide(", in: model)
        let stamp = decide.range(of: "defaults.set(true, forKey: Self.askedKey)")
        let call = decide.range(of: "request()")
        expect(stamp != nil && call != nil && stamp!.lowerBound < call!.lowerBound,
            "the stamp precedes the request in the source")
        let ask = body(of: "func askIfNeverAsked(", in: model)
        expect(ask.contains("Task.sleep(for: delay)"), "the ask waits its second")
        expect(ask.contains("conditions()"), "and reads the conditions after it")

        let store = read("Sources/Passband/Model/AppStore.swift")
        let hook = body(of: "func askDefaultMailIfNeverAsked() {", in: store)
        expect(hook.contains("self.connStatus == .connected && self.connectionVerified"),
            "the store passes a VERIFIED connection")
        expect(hook.contains("AgentNudge.othersShowing(self)"),
            "and blocks on the veil, What's New, the iPhone, share and update cards")
        expect(hook.contains("AgentNudge.shared.showingNudge"), "and on the agent ad")

        let firstLoad = read("Sources/Passband/Model/FirstLoad.swift")
        let conclude = body(of: "private func conclude(_ store: AppStore) {", in: firstLoad)
        expect(conclude.contains("if firstTime { store.askDefaultMailIfNeverAsked() }"),
            "a new install is asked when its first look ends")
        let root = read("Sources/Passband/App/RootView.swift")
        expect(root.contains("store.askDefaultMailIfNeverAsked()"),
            "an existing install is asked at the first sync of a launch")
    }

    /// The card this replaced is gone, everywhere.
    static func noCardRemains() {
        let fm = FileManager.default
        expect(!fm.fileExists(atPath: "Sources/Passband/Views/DefaultMailNudgeCard.swift"),
            "the card's file is gone")
        guard let walk = fm.enumerator(atPath: "Sources") else {
            expect(false, "walk Sources (run from passband/)")
            return
        }
        let gone = ["DefaultMailNudgeCard", "default_mail_nudge_closed", "cornerOccupied",
                    "DefaultMailApp.shared.showingNudge", "noteShown("]
        for case let path as String in walk where path.hasSuffix(".swift") {
            let source = read("Sources/" + path)
            for name in gone {
                expect(!source.contains(name), "\(name) is gone from \(path)")
            }
        }
        let project = read("Passband.xcodeproj/project.pbxproj")
        expect(!project.contains("DefaultMailNudgeCard"), "and from the Xcode project")
    }

    static func expect(_ cond: Bool, _ what: String) {
        checks += 1
        if !cond {
            failures += 1
            print("  FAIL: \(what)")
        }
    }
}
