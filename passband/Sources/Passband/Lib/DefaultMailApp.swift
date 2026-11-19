// PASSBAND AS THE MAC'S DEFAULT MAIL APP: whether it is, the one time it asks
// to be, and the button that makes it so.
//
// NEW USERS ARE ASKED DURING ONBOARDING, BY THE SYSTEM ALONE. The moment the
// first look at a newly connected mailbox ends (FirstLoad.conclude, the inbox
// on screen, the connection verified), and a beat later so the dialog does
// not land mid-transition, Passband asks macOS to make it the default. macOS
// shows its own "change default email reader?" confirmation; there is no
// card, sheet or step of ours. The stamp is written BEFORE the request, so a
// crash or a refusal never asks again, and a fresh install therefore never
// sees the card below.
//
// EXISTING USERS (onboarded before this version) never pass through that
// moment again, so they keep the card, earned by a send:
//
// THE CARD IS EARNED BY A SEND, not by a launch. Somebody who has just sent
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

    /// `PASSBAND_FORCE_DEFAULT_MAIL_NUDGE=onboarding` runs the onboarding ask
    /// at the first sync of any launch, past the stamp and without writing
    /// it, so the system's dialog can be looked at without a fresh install.
    /// It still skips when Passband is already the default.
    nonisolated static let forcedOnboarding =
        ProcessInfo.processInfo.environment["PASSBAND_FORCE_DEFAULT_MAIL_NUDGE"] == "onboarding"

    /// How long after the inbox first shows before the system is asked.
    nonisolated static let onboardingDelay: Duration = .seconds(1)

    private let defaults: UserDefaults
    /// Launch Services' answer: is Passband the app mailto: links open in.
    /// Injected so a suite can answer without the system.
    private let probe: () -> Bool
    private let isForced: Bool
    /// Asks the system to make Passband the default. Injected so a suite can
    /// see the call, and what was stamped when it came.
    private let request: () -> Void

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
        probe: @escaping () -> Bool = DefaultMailApp.systemSaysPassband,
        request: (() -> Void)? = nil
    ) {
        self.defaults = defaults
        self.probe = probe
        self.request = request ?? { DefaultMailApp.shared.makeDefault() }
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

    /// THE ONBOARDING RULE, pure: the end of a first-ever onboarding on this
    /// Mac, over a verified connection, on an install never asked and not
    /// already the default.
    nonisolated static func asksDuringOnboarding(
        firstOnboarding: Bool, verified: Bool, isDefault: Bool, asked: Bool
    ) -> Bool {
        firstOnboarding && verified && !isDefault && !asked
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

    /// The first look at a newly connected mailbox just ended. On a first
    /// onboarding, asks the system (no card), once ever: the stamp is written
    /// here, before the request, and the request comes `after` later. `forced`
    /// (the preview override) skips the first-onboarding and stamp checks and
    /// writes nothing. Returns whether it asked.
    @discardableResult
    func askDuringOnboarding(
        firstOnboarding: Bool, verified: Bool, forced: Bool = false,
        after delay: Duration? = DefaultMailApp.onboardingDelay
    ) -> Bool {
        guard !FixtureMode.isEnabled || forced else { return false }
        refresh()
        guard
            Self.asksDuringOnboarding(
                firstOnboarding: firstOnboarding || forced, verified: verified,
                isDefault: isDefault, asked: asked && !forced)
        else { return false }
        if !forced { defaults.set(true, forKey: Self.askedKey) }
        guard let delay else {
            request()
            return true
        }
        Task { @MainActor in
            try? await Task.sleep(for: delay)
            self.request()
        }
        return true
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
