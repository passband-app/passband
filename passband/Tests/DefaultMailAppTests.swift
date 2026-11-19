// The one ask to be the default mail app: the system's dialog alone, once
// ever, the first time an inbox loads over a verified connection, whether the
// install is new or from before the ask, and only from a bundle that claims
// mailto:. The rule is pure; the model runs against a throwaway UserDefaults
// suite, a probe standing in for Launch Services, a stubbed bundle plist and
// a request that records what was stamped when it came. The wiring a
// headless suite cannot run is pinned by reading the source, and so is the
// absence of the card this replaced.

import Foundation

@main
@MainActor
struct DefaultMailAppTests {
    static var failures = 0
    static var checks = 0

    static func main() async {
        theRule()
        whoIsTheDefault()
        onlyABundleThatClaimsMailtoAsks()
        aDevBuildSpendsNothing()
        aNewInstallIsAskedOnce()
        anExistingInstallIsAskedOnceOnUpdate()
        theStampComesBeforeTheRequest()
        skippedWhenAlreadyTheDefault()
        blockedWaitsForTheNextLaunch()
        theOverride()
        await theDelayedPathRequestsLater()
        await twoCallersInOneSecondMakeOneRequest()
        theWiringIsThere()
        theOtherCardsWait()
        noCardRemains()

        if failures > 0 {
            print("FAILED: \(failures) of \(checks) checks")
            exit(1)
        }
        print("ok: \(checks) checks passed")
    }

    static func rule(
        declares: Bool = true, verified: Bool = true, blocked: Bool = false,
        isDefault: Bool = false, asked: Bool = false, forced: Bool = false
    ) -> Bool {
        DefaultMailApp.shouldAsk(
            declaresMailto: declares, verified: verified, blocked: blocked, isDefault: isDefault,
            asked: asked, forced: forced)
    }

    static func theRule() {
        expect(rule(), "claims mailto:, verified, nothing in the way, not the default, never asked: asks")
        expect(!rule(declares: false), "never from a bundle that does not claim mailto:")
        expect(!rule(declares: false, forced: true), "not even forced")
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

    /// Bundle plists, stubbed: the release one, and the one build.sh leaves
    /// a dev build.
    nonisolated(unsafe) static let releasePlist: [String: Any] = [
        "CFBundleURLTypes": [
            ["CFBundleURLName": "app.passband.client.pair", "CFBundleURLSchemes": ["passband"]],
            ["CFBundleURLName": "app.passband.client.mailto", "CFBundleURLSchemes": ["mailto"]],
        ]
    ]
    nonisolated(unsafe) static let devPlist: [String: Any] = [
        "CFBundleURLTypes": [
            ["CFBundleURLName": "app.passband.client.pair", "CFBundleURLSchemes": ["passband"]]
        ]
    ]

    static func onlyABundleThatClaimsMailtoAsks() {
        expect(DefaultMailApp.declaresMailto(releasePlist), "the release plist claims mailto:")
        expect(!DefaultMailApp.declaresMailto(devPlist), "a dev build's plist does not")
        expect(!DefaultMailApp.declaresMailto(nil), "no plist claims nothing")
        expect(!DefaultMailApp.declaresMailto(["CFBundleURLTypes": "mailto"]), "a malformed one neither")
        expect(
            DefaultMailApp.declaresMailto(["CFBundleURLTypes": [["CFBundleURLSchemes": ["MAILTO"]]]]),
            "the scheme compares without case")
        // The source plist is the release one; build.sh strips it from the rest.
        if let data = FileManager.default.contents(atPath: "Sources/Passband/Info.plist"),
            let plist = try? PropertyListSerialization.propertyList(from: data, format: nil)
                as? [String: Any]
        {
            expect(DefaultMailApp.declaresMailto(plist), "Sources/Passband/Info.plist claims mailto:")
        } else {
            expect(false, "read Sources/Passband/Info.plist (run from passband/)")
        }
    }

    // MARK: - the model

    static func fresh() -> UserDefaults {
        let name = "DefaultMailAppTests.\(UUID().uuidString)"
        let d = UserDefaults(suiteName: name)!
        d.removePersistentDomain(forName: name)
        return d
    }

    /// One Mac's defaults domain, shared by every build on it as the real one
    /// is: a Launch Services answer, and a count of requests with whether the
    /// stamp was down at each.
    @MainActor
    final class Install {
        let defaults: UserDefaults
        var isDefault = false
        var requests = 0
        var stampedAtRequest: [Bool] = []

        init(_ defaults: UserDefaults) { self.defaults = defaults }

        /// A launch of a build: a new model over the same defaults.
        func launch(forced: Bool = false, dev: Bool = false) -> DefaultMailApp {
            let plist = dev ? DefaultMailAppTests.devPlist : DefaultMailAppTests.releasePlist
            return DefaultMailApp(
                defaults: defaults, forced: forced, probe: { [unowned self] in self.isDefault },
                request: { [unowned self] in
                    self.requests += 1
                    self.stampedAtRequest.append(self.defaults.bool(forKey: DefaultMailApp.askedKey))
                },
                declaresMailto: { DefaultMailApp.declaresMailto(plist) })
        }

        /// The two places the app asks, as they are wired (pinned in
        /// `theWiringIsThere`): the end of a first look asks only on the
        /// first one; the first sync of a launch always.
        @discardableResult
        func firstLookEnded(_ m: DefaultMailApp, firstTime: Bool, after: Duration? = nil) -> Bool {
            firstTime ? m.askIfNeverAsked(verified: true, blocked: false, after: after) : false
        }
        @discardableResult
        func firstSync(
            _ m: DefaultMailApp, verified: Bool = true, blocked: Bool = false, after: Duration? = nil
        ) -> Bool {
            m.askIfNeverAsked(verified: verified, blocked: blocked, after: after)
        }
    }

    static func newInstall() -> Install { Install(fresh()) }

    /// An install from before this version: onboarded long ago (the first
    /// look stamped), release notes read up to an older build, and no trace
    /// of the ask, which did not exist then.
    static func existingInstall() -> Install {
        let d = fresh()
        d.set(true, forKey: "passband.pref.tourCompleted")
        d.set("0.0.19", forKey: "passband.pref.lastSeenReleaseNotes")
        return Install(d)
    }

    static func aDevBuildSpendsNothing() {
        let mac = existingInstall()
        let dev = mac.launch(dev: true)
        expect(!mac.firstSync(dev), "a dev build never asks")
        expect(mac.requests == 0, "and requests nothing")
        expect(!mac.defaults.bool(forKey: DefaultMailApp.askedKey), "and writes no stamp")
        expect(!dev.askedThisLaunch, "and holds no cards")
        // The release build on the same Mac still has its one ask.
        expect(mac.firstSync(mac.launch()), "the release build, same defaults, still asks")
        expect(mac.requests == 1, "once")
    }

    static func aNewInstallIsAskedOnce() {
        let i = newInstall()
        let m = i.launch()
        expect(!i.firstSync(m, verified: false), "a new install waits for verification")
        expect(i.firstLookEnded(m, firstTime: true), "its first look ending asks")
        expect(i.requests == 1, "one request")
        expect(!i.firstSync(m), "the same launch never asks twice")
        let next = i.launch()
        expect(!i.firstLookEnded(next, firstTime: false) && !i.firstSync(next), "nor the next launch")
        expect(i.requests == 1, "still one request, ever")
    }

    static func anExistingInstallIsAskedOnceOnUpdate() {
        let i = existingInstall()
        let m = i.launch()
        // Its first look ended long ago: that road never runs for it.
        expect(!i.firstLookEnded(m, firstTime: false), "no first look on an update")
        expect(i.requests == 0, "nothing from that road")
        // Its first sync after the update asks, before What's New (which has
        // notes to show it) is offered, and the launch then holds the cards.
        expect(i.firstSync(m), "the update's first sync asks")
        expect(m.askedThisLaunch, "and holds What's New and the ads for the launch")
        expect(i.requests == 1, "once")
        // The next launch: no ask, and the cards are free again.
        let next = i.launch()
        expect(!i.firstSync(next) && !next.askedThisLaunch, "never on a later launch")
        expect(i.requests == 1, "still once")
    }

    static func theStampComesBeforeTheRequest() {
        let i = newInstall()
        i.firstSync(i.launch())
        expect(i.stampedAtRequest == [true], "the stamp was down when the request went")
        expect(i.defaults.bool(forKey: DefaultMailApp.askedKey), "stamped for good")
    }

    static func skippedWhenAlreadyTheDefault() {
        let i = existingInstall()
        i.isDefault = true
        let m = i.launch()
        expect(!i.firstSync(m), "already the default: no ask")
        expect(i.requests == 0 && !m.askedThisLaunch, "no request, no cards held")
        expect(!i.defaults.bool(forKey: DefaultMailApp.askedKey), "and no stamp")
        i.isDefault = false
        expect(i.firstSync(i.launch()), "no longer the default, never asked: asks")
    }

    static func blockedWaitsForTheNextLaunch() {
        let i = existingInstall()
        let m = i.launch()
        expect(!i.firstSync(m, blocked: true), "another card up: no ask")
        expect(i.requests == 0 && !i.defaults.bool(forKey: DefaultMailApp.askedKey),
            "nothing requested, nothing stamped")
        expect(!m.askedThisLaunch, "nothing held for a launch that did not ask")
        expect(i.firstSync(i.launch()), "the next launch asks")
        expect(i.requests == 1, "once")
    }

    static func theOverride() {
        let i = existingInstall()
        i.defaults.set(true, forKey: DefaultMailApp.askedKey)
        expect(i.firstSync(i.launch(forced: true)), "forced asks on an asked install")
        expect(i.requests == 1, "forced requests")
        let j = newInstall()
        let f = j.launch(forced: true)
        j.firstSync(f)
        expect(!j.defaults.bool(forKey: DefaultMailApp.askedKey), "forced writes no stamp")
        expect(!j.firstSync(f), "and asks once per launch")
        j.isDefault = true
        expect(!j.firstSync(j.launch(forced: true)), "forced still skips the default")
    }

    /// The real delayed path: decided and stamped at once, requested later.
    static func theDelayedPathRequestsLater() async {
        let i = newInstall()
        let m = i.launch()
        expect(i.firstSync(m, after: .milliseconds(40)), "decides at once")
        expect(i.defaults.bool(forKey: DefaultMailApp.askedKey), "stamped at once")
        expect(i.requests == 0, "the request waits")
        try? await Task.sleep(for: .milliseconds(250))
        expect(i.requests == 1, "and goes after the delay")
        expect(i.stampedAtRequest == [true], "with the stamp already down")
    }

    /// A new install whose first look ends in the same second as the first
    /// sync: both roads call, one request.
    static func twoCallersInOneSecondMakeOneRequest() async {
        let i = newInstall()
        let m = i.launch()
        let a = i.firstLookEnded(m, firstTime: true, after: .milliseconds(40))
        let b = i.firstSync(m, after: .milliseconds(40))
        expect(a && !b, "the first caller asks, the second does not")
        try? await Task.sleep(for: .milliseconds(250))
        expect(i.requests == 1, "exactly one request")
        // The same with the override, which skips the stamp but not the launch.
        let j = newInstall()
        let f = j.launch(forced: true)
        j.firstLookEnded(f, firstTime: true, after: .milliseconds(40))
        j.firstSync(f, after: .milliseconds(40))
        try? await Task.sleep(for: .milliseconds(250))
        expect(j.requests == 1, "forced too: exactly one request")
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
        let ask = body(of: "func askIfNeverAsked(", in: model)
        let stamp = ask.range(of: "defaults.set(true, forKey: Self.askedKey)")
        let call = ask.range(of: "request()")
        expect(stamp != nil && call != nil && stamp!.lowerBound < call!.lowerBound,
            "the stamp precedes the request in the source")
        expect(ask.contains("declaresMailto: declaresMailto()"), "the bundle's own claim is checked")
        expect(model.contains("DefaultMailApp.declaresMailto(Bundle.main.infoDictionary)"),
            "against the RUNNING bundle's plist")

        let store = read("Sources/Passband/Model/AppStore.swift")
        let hook = body(of: "func askDefaultMailIfNeverAsked() -> Bool {", in: store)
        expect(hook.contains("verified: connStatus == .connected && connectionVerified"),
            "the store passes a VERIFIED connection")
        expect(hook.contains("AgentNudge.othersShowing(self)"),
            "and blocks on the veil, What's New, the iPhone, share and update cards")
        expect(hook.contains("AgentNudge.shared.showingNudge"), "and on the agent ad")

        let firstLoad = read("Sources/Passband/Model/FirstLoad.swift")
        let conclude = body(of: "private func conclude(_ store: AppStore) {", in: firstLoad)
        expect(conclude.contains("if firstTime { store.askDefaultMailIfNeverAsked() }"),
            "a new install is asked when its first look ends, and only then")
        let root = read("Sources/Passband/App/RootView.swift")
        guard let sync = root.range(of: "if old == nil, new != nil {") else {
            expect(false, "find the first-sync block")
            return
        }
        let block = root[sync.upperBound...]
        let askAt = block.range(of: "store.askDefaultMailIfNeverAsked()")
        let notesAt = block.range(of: "store.whatsNew.maybeShow()")
        let adAt = block.range(of: "AgentNudge.shared.askIfEarned(store: store)")
        expect(
            askAt != nil && notesAt != nil && adAt != nil
                && askAt!.lowerBound < notesAt!.lowerBound && askAt!.lowerBound < adAt!.lowerBound,
            "at the first sync the ask is decided before What's New and the agent ad")
    }

    /// A launch that asked holds the other one-time cards.
    static func theOtherCardsWait() {
        let notes = read("Sources/Passband/Model/WhatsNew.swift")
        expect(body(of: "func maybeShow() {", in: notes).contains("!DefaultMailApp.holdsCardsThisLaunch"),
            "What's New holds")
        let agent = read("Sources/Passband/Views/AgentNudgeModal.swift")
        expect(
            body(of: "func askIfEarned(store: AppStore) {", in: agent)
                .contains("!DefaultMailApp.holdsCardsThisLaunch"), "the agent ad holds")
        let root = read("Sources/Passband/App/RootView.swift")
        expect(root.components(separatedBy: "guard !DefaultMailApp.holdsCardsThisLaunch").count - 1 == 2,
            "the iPhone ad's two triggers hold")
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
        var swiftFiles = 0
        var hits: [String] = []
        for case let path as String in walk where path.hasSuffix(".swift") {
            swiftFiles += 1
            let source = read("Sources/" + path)
            for name in gone where source.contains(name) { hits.append("\(name) in \(path)") }
        }
        expect(swiftFiles > 100, "the walk saw the sources")
        expect(hits.isEmpty, "no card code remains: \(hits)")
        let project = read("Passband.xcodeproj/project.pbxproj")
        expect(!project.contains("DefaultMailNudgeCard"), "and none in the Xcode project")
    }

    static func expect(_ cond: Bool, _ what: String) {
        checks += 1
        if !cond {
            failures += 1
            print("  FAIL: \(what)")
        }
    }
}
