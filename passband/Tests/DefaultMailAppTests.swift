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

    static func expect(_ cond: Bool, _ what: String) {
        checks += 1
        if !cond {
            failures += 1
            print("  FAIL: \(what)")
        }
    }
}
