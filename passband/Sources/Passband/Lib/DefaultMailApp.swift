// PASSBAND AS THE MAC'S DEFAULT MAIL APP: whether it is, the one time it asks
// to be, and the button that makes it so.
//
// THE ASK IS EARNED BY A SEND, not by a launch. Somebody who has just sent
// mail from Passband has a reason to want email links to land here; somebody
// who has only opened the app has not. So the card can only be wanted right
// after a send from Passband goes out: the first one on a new install, and
// the first one after this update on an install that predates it (the stamp
// is new, so neither has been asked).
//
// ONLY IF IT IS NOT ALREADY THE DEFAULT. Asked of Launch Services at the
// moment of the send, by bundle id, so a person who set it in Mail's
// settings, or from Settings here, is never asked.
//
// WANTED IS NOT DRAWN. The card waits while a composer is open (it sits in
// the corner the Send button is in, and the next mail is often started right
// after the last one went) and while the sitrep's corner holds the
// unsubscribe tray or the smart clear receipt. It comes back when they go.
//
// ASKED ONCE, EVER, like the other one-time cards: the stamp is written the
// first time the card is actually DRAWN (`noteShown`), before any answer, so
// a crash with it on screen has still asked. A card wanted and never drawn
// (quit with a composer open) is not an ask; the next send earns it again.
// "Not now" is an answer. Settings > Mail keeps the button for anyone who
// changes their mind.
//
// NEVER ON TOP OF ANOTHER ONE-TIME CARD. A send while one is up wants
// nothing; the next send gets the chance instead.
//
// "Make default" is a request: macOS may ask the person to confirm it, and it
// can fail. What Launch Services says afterwards is the answer, re-read
// rather than assumed.

import Foundation
import Observation

#if os(macOS)
    import AppKit
#endif

@MainActor
@Observable
final class DefaultMailApp {
    static let shared = DefaultMailApp()

    nonisolated static let askedKey = "passband.defaultMail.asked"

    /// `PASSBAND_FORCE_DEFAULT_MAIL_NUDGE=1` puts the card up at launch without
    /// recording an ask, so it can be looked at without sending mail. Same
    /// posture as the other cards' overrides: a launch environment nobody sets
    /// by accident.
    nonisolated static let forced =
        ProcessInfo.processInfo.environment["PASSBAND_FORCE_DEFAULT_MAIL_NUDGE"] == "1"

    private let defaults: UserDefaults
    /// Launch Services' answer: is Passband the app mailto: links open in.
    /// Injected so a suite can answer without the system.
    private let probe: () -> Bool
    private let isForced: Bool

    /// Whether the card is WANTED: earned and not yet answered. Whether it is
    /// drawn right now is `drawn(composerOpen:cornerOccupied:)`.
    var showingNudge = false

    /// What Launch Services last said. Refreshed on `refresh()`, which the
    /// Settings row calls on appear and on every return to the app.
    private(set) var isDefault = false

    /// The last "Make default" failed or was declined.
    private(set) var lastRequestFailed = false

    init(
        defaults: UserDefaults = .standard,
        forced: Bool = DefaultMailApp.forced,
        probe: @escaping () -> Bool = DefaultMailApp.systemSaysPassband
    ) {
        self.defaults = defaults
        self.probe = probe
        self.isForced = forced
        refresh()
        showingNudge = forced
    }

    /// THE RULE, pure. `sentFromPassband` is whether this moment follows a
    /// send that went out; nothing else is a moment to ask at.
    nonisolated static func earned(
        sentFromPassband: Bool, isDefault: Bool, asked: Bool, othersShowing: Bool
    ) -> Bool {
        sentFromPassband && !isDefault && !asked && !othersShowing
    }

    /// THE PLACEMENT RULE, pure: a wanted card is drawn only with no composer
    /// open and nothing else in the bottom-right corner.
    nonisolated static func drawn(wanted: Bool, composerOpen: Bool, cornerOccupied: Bool) -> Bool {
        wanted && !composerOpen && !cornerOccupied
    }

    /// The default handler's bundle id against this app's, both lowercased.
    /// Nil (no handler at all, or a handler with no id) is not Passband.
    nonisolated static func isPassband(handlerBundleId: String?, ownBundleId: String?) -> Bool {
        guard let handler = handlerBundleId?.lowercased(), let own = ownBundleId?.lowercased(),
            !own.isEmpty
        else { return false }
        return handler == own
    }

    var asked: Bool { defaults.bool(forKey: Self.askedKey) }

    func drawn(composerOpen: Bool, cornerOccupied: Bool) -> Bool {
        Self.drawn(wanted: showingNudge, composerOpen: composerOpen, cornerOccupied: cornerOccupied)
    }

    /// A send from Passband just went out. Wants the card if this is the
    /// moment. The one entry point the store calls (`release`).
    func noteSent(othersShowing: Bool) {
        guard !FixtureMode.isEnabled, !showingNudge else { return }
        refresh()
        guard
            Self.earned(
                sentFromPassband: true, isDefault: isDefault, asked: asked,
                othersShowing: othersShowing)
        else { return }
        showingNudge = true
    }

    /// The card is on screen. Stamps the ask the first time; a forced card
    /// (the preview override) stamps nothing.
    func noteShown() {
        guard showingNudge, !isForced, !asked else { return }
        defaults.set(true, forKey: Self.askedKey)
    }

    /// Either answer. Never wanted again.
    func close() {
        showingNudge = false
    }

    /// Re-read the system's answer.
    func refresh() {
        isDefault = probe()
    }

    /// Ask the system to make Passband the mailto: handler. macOS may ask the
    /// person to confirm; `isDefault` follows whatever it decided.
    func makeDefault() {
        #if os(macOS)
            lastRequestFailed = false
            NSWorkspace.shared.setDefaultApplication(
                at: Bundle.main.bundleURL, toOpenURLsWithScheme: "mailto"
            ) { error in
                Task { @MainActor in
                    let shared = DefaultMailApp.shared
                    shared.refresh()
                    shared.lastRequestFailed = error != nil && !shared.isDefault
                }
            }
        #endif
    }

    /// Launch Services' handler for mailto:, compared by bundle id. Always
    /// false off the Mac.
    nonisolated static func systemSaysPassband() -> Bool {
        #if os(macOS)
            guard let probeURL = URL(string: "mailto:someone@example.com"),
                let handler = NSWorkspace.shared.urlForApplication(toOpen: probeURL)
            else { return false }
            return isPassband(
                handlerBundleId: Bundle(url: handler)?.bundleIdentifier,
                ownBundleId: Bundle.main.bundleIdentifier)
        #else
            return false
        #endif
    }
}
