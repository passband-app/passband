// The default-mail-app ask, and every way it must NOT fire. The rules are
// pure; the model runs against a throwaway UserDefaults suite and a probe
// standing in for Launch Services. The store's and the view's wiring, which
// a headless suite cannot run, is pinned by reading the source: removing the
// feature fails here, not only at compile time.

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
        waitsForTheComposerAndTheCorner()
        theModelStampsOnceWhenDrawn()
        theWiringIsThere()
        onboardingAsksTheSystemOnce()

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

    static func waitsForTheComposerAndTheCorner() {
        expect(
            DefaultMailApp.drawn(wanted: true, composerOpen: false, cornerOccupied: false),
            "wanted, nothing in the way: drawn")
        expect(
            !DefaultMailApp.drawn(wanted: true, composerOpen: true, cornerOccupied: false),
            "never over an open composer and its Send button")
        expect(
            !DefaultMailApp.drawn(wanted: true, composerOpen: false, cornerOccupied: true),
            "never over the unsubscribe tray or the smart clear receipt")
        expect(
            !DefaultMailApp.drawn(wanted: false, composerOpen: false, cornerOccupied: false),
            "not wanted, not drawn")
    }

    static func fresh() -> UserDefaults {
        let name = "DefaultMailAppTests.\(UUID().uuidString)"
        let d = UserDefaults(suiteName: name)!
        d.removePersistentDomain(forName: name)
        return d
    }

    static func theModelStampsOnceWhenDrawn() {
        // A send on an install that is not the default: wanted, not yet asked.
        let d = fresh()
        var isDefault = false
        let m = DefaultMailApp(defaults: d, forced: false, probe: { isDefault })
        expect(!m.showingNudge && !m.asked, "nothing at launch")
        m.noteSent(othersShowing: false)
        expect(m.showingNudge, "a send wants the card")
        expect(!m.asked, "wanted is not asked: nothing stamped until it is drawn")
        // Hidden behind a composer, then back: still wanted, still one stamp.
        expect(!m.drawn(composerOpen: true, cornerOccupied: false), "hidden by a composer")
        expect(m.drawn(composerOpen: false, cornerOccupied: false), "back when it closes")
        m.noteShown()
        expect(m.asked && d.bool(forKey: DefaultMailApp.askedKey), "drawn is asked")
        m.noteShown()
        expect(m.asked, "drawn again stamps nothing new")
        // Not now.
        m.close()
        expect(!m.showingNudge, "closed")
        m.noteSent(othersShowing: false)
        expect(!m.showingNudge, "never again after an answer")

        // Already the default: a send wants nothing.
        isDefault = true
        let m2 = DefaultMailApp(defaults: fresh(), forced: false, probe: { isDefault })
        m2.noteSent(othersShowing: false)
        expect(!m2.showingNudge, "the default is never asked")
        // The system's answer is re-read at the send, not cached from launch.
        isDefault = false
        m2.noteSent(othersShowing: false)
        expect(m2.showingNudge, "no longer the default: asked at the next send")

        // Another card up: nothing wanted, nothing stamped.
        let m3 = DefaultMailApp(defaults: fresh(), forced: false, probe: { false })
        m3.noteSent(othersShowing: true)
        expect(!m3.showingNudge && !m3.asked, "never over another one-time card")

        // Wanted, never drawn (quit with a composer open): not an ask.
        let d4 = fresh()
        let m4 = DefaultMailApp(defaults: d4, forced: false, probe: { false })
        m4.noteSent(othersShowing: false)
        let m4b = DefaultMailApp(defaults: d4, forced: false, probe: { false })
        m4b.noteSent(othersShowing: false)
        expect(m4b.showingNudge, "a card never drawn is earned again by the next send")

        // The preview override draws it and stamps nothing.
        let m5 = DefaultMailApp(defaults: fresh(), forced: true, probe: { false })
        expect(m5.showingNudge, "forced is up at launch")
        m5.noteShown()
        expect(!m5.asked, "forced stamps nothing")
    }

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
        let ends = ["\n    func ", "\n    private func ", "\n    var ", "\n    // MARK:"]
            .compactMap { rest.range(of: $0)?.lowerBound }
        return String(rest[..<(ends.min() ?? rest.endIndex)])
    }

    static func theWiringIsThere() {
        let store = read("Sources/Passband/Model/AppStore.swift")
        let release = body(of: "private func release(_ key: UUID) async {", in: store)
        let sentCase = release.range(of: "case .sent(let result):")
        let call = release.range(of: "DefaultMailApp.shared.noteSent(")
        expect(call != nil, "a send that went out tells DefaultMailApp (release)")
        expect(
            sentCase != nil && call != nil && sentCase!.lowerBound < call!.lowerBound,
            "and only from the same-account .sent branch")

        let receive = body(of: "func receiveMailtoURL(", in: store)
        expect(receive.contains("mailtoInbox.add("), "a link from another app is held in the inbox")
        expect(receive.contains("MailtoInbox.copy("), "a refused link says so")
        let settle = body(of: "func settleMailtoInbox(", in: store)
        expect(
            settle.contains("connStatus == .connected && connectionVerified"),
            "held links wait for a VERIFIED connection")
        expect(settle.contains("gate: connStatus == .disconnected"), "the Connect screen drops them")
        expect(settle.contains("openComposeMailto(href)"), "and open through the in-message path")
        expect(settle.contains("pushToast(MailtoInbox.copy("), "a drop is announced")
        expect(settle.contains("scheduleMailtoInboxExpiry()"), "the hold has a clock")
        let verified = body(of: "private func noteConnectionVerified() {", in: store)
        expect(verified.contains("settleMailtoInbox()"), "verification flushes held links")
        let open = body(of: "func openComposeMailto(", in: store)
        expect(open.contains("Couldn't open that email link."), "an unreadable link says so")
        expect(open.contains("while a message is sending"), "a link during a send says so")

        let app = read("Sources/Passband/App/PassbandApp.swift")
        expect(app.contains("store.receiveMailtoURL(url)"), "onOpenURL routes mailto: to the store")
        let plist = read("Sources/Passband/Info.plist")
        expect(plist.contains("<string>mailto</string>"), "the bundle declares mailto:")
        let build = read("build.sh")
        expect(
            build.contains("if [[ \"$MODE\" != \"release\" ]]")
                && build.contains("app.passband.client.mailto"),
            "only a release build claims mailto: (build.sh strips it from the rest)")

        let card = read("Sources/Passband/Views/DefaultMailNudgeCard.swift")
        expect(card.contains("model.drawn("), "the card asks the placement rule")
        expect(card.contains("store.composeOpen"), "and passes whether a composer is open")
        expect(card.contains("store.bottomTrailingCornerOccupied"), "and whether the corner is taken")
        expect(card.contains("model.noteShown()"), "and stamps when drawn")
        let root = read("Sources/Passband/App/RootView.swift")
        expect(root.contains(".overlay { DefaultMailNudgeCard() }"), "the card is mounted")
    }

    /// New users: macOS is asked once at the end of the first onboarding,
    /// with the stamp already written when the request goes, and never the
    /// card after. Existing users: no onboarding ask, the card as before.
    static func onboardingAsksTheSystemOnce() {
        // The rule.
        expect(
            DefaultMailApp.asksDuringOnboarding(
                firstOnboarding: true, verified: true, isDefault: false, asked: false),
            "a first onboarding, verified, not the default: asks")
        expect(
            !DefaultMailApp.asksDuringOnboarding(
                firstOnboarding: false, verified: true, isDefault: false, asked: false),
            "an install onboarded before never gets the onboarding ask")
        expect(
            !DefaultMailApp.asksDuringOnboarding(
                firstOnboarding: true, verified: false, isDefault: false, asked: false),
            "not before the connection is verified")
        expect(
            !DefaultMailApp.asksDuringOnboarding(
                firstOnboarding: true, verified: true, isDefault: true, asked: false),
            "already the default: skipped")
        expect(
            !DefaultMailApp.asksDuringOnboarding(
                firstOnboarding: true, verified: true, isDefault: false, asked: true),
            "asked once is asked forever")

        // The model: the request is made with the stamp already written.
        let d = fresh()
        var requests = 0
        var stampedAtRequest: Bool?
        let m = DefaultMailApp(
            defaults: d, forced: false, probe: { false },
            request: {
                requests += 1
                stampedAtRequest = d.bool(forKey: DefaultMailApp.askedKey)
            })
        expect(m.askDuringOnboarding(firstOnboarding: true, verified: true, after: nil), "asked")
        expect(requests == 1, "the system was asked once")
        expect(stampedAtRequest == true, "the stamp was written BEFORE the request")
        expect(!m.showingNudge, "and no card of ours")
        // Never again: a second first look (a crash, a relaunch) asks nothing.
        expect(!m.askDuringOnboarding(firstOnboarding: true, verified: true, after: nil), "once")
        expect(requests == 1, "still one request")
        // The card never follows an onboarding ask, whatever happens to it.
        m.noteSent(othersShowing: false)
        expect(!m.showingNudge, "no card after the onboarding ask")

        // Already the default: no request, no stamp.
        var r2 = 0
        let d2 = fresh()
        let m2 = DefaultMailApp(defaults: d2, forced: false, probe: { true }, request: { r2 += 1 })
        expect(!m2.askDuringOnboarding(firstOnboarding: true, verified: true, after: nil), "skipped")
        expect(r2 == 0 && !d2.bool(forKey: DefaultMailApp.askedKey), "nothing asked, nothing stamped")

        // An existing user: the end of a first look that is not their first
        // onboarding asks nothing, and the send card is still theirs.
        var r3 = 0
        let m3 = DefaultMailApp(defaults: fresh(), forced: false, probe: { false }, request: { r3 += 1 })
        expect(!m3.askDuringOnboarding(firstOnboarding: false, verified: true, after: nil), "no ask")
        expect(r3 == 0, "no request")
        m3.noteSent(othersShowing: false)
        expect(m3.showingNudge, "existing users still get the card after a send")

        // The preview override: asks past the stamp and writes none.
        var r4 = 0
        let d4 = fresh()
        d4.set(true, forKey: DefaultMailApp.askedKey)
        let m4 = DefaultMailApp(defaults: d4, forced: false, probe: { false }, request: { r4 += 1 })
        expect(
            m4.askDuringOnboarding(firstOnboarding: false, verified: true, forced: true, after: nil),
            "forced asks even on an asked install")
        expect(r4 == 1, "forced requests")
        let d5 = fresh()
        let m5 = DefaultMailApp(defaults: d5, forced: false, probe: { false }, request: {})
        m5.askDuringOnboarding(firstOnboarding: false, verified: true, forced: true, after: nil)
        expect(!d5.bool(forKey: DefaultMailApp.askedKey), "forced stamps nothing")

        // The wiring: the end of the first look calls it with the real
        // first-time flag and the verified signal.
        let firstLoad = read("Sources/Passband/Model/FirstLoad.swift")
        let conclude = body(of: "private func conclude(_ store: AppStore) {", in: firstLoad)
        expect(
            conclude.contains("DefaultMailApp.shared.askDuringOnboarding(")
                && conclude.contains("firstOnboarding: firstTime, verified: store.connectionVerified"),
            "FirstLoad.conclude asks on a first onboarding, verified")
        let model = read("Sources/Passband/Lib/DefaultMailApp.swift")
        let ask = body(of: "func askDuringOnboarding(", in: model)
        let stamp = ask.range(of: "defaults.set(true, forKey: Self.askedKey)")
        let call = ask.range(of: "request()")
        expect(
            stamp != nil && call != nil && stamp!.lowerBound < call!.lowerBound,
            "the stamp precedes the request in the source too")
        expect(ask.contains("Task.sleep(for: delay)"), "the request waits a beat")
        expect(DefaultMailApp.onboardingDelay == .seconds(1), "about a second")
    }

    static func expect(_ cond: Bool, _ what: String) {
        checks += 1
        if !cond {
            failures += 1
            print("  FAIL: \(what)")
        }
    }
}
