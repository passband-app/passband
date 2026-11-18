// PASSBAND AS THE MAC'S DEFAULT MAIL APP: whether it is, the one time it asks
// to be, and the button that makes it so.
//
// THE ASK COMES AFTER A SEND, not at launch. Somebody who has just sent mail
// from Passband has a reason to want email links to land here; somebody who
// has only opened the app has not. So the card can only go up right after a
// send from Passband succeeds: the first one on a new install, and the first
// one after this update on an install that predates it (the stamp is new, so
// neither has been asked).
//
// ONLY IF IT IS NOT ALREADY THE DEFAULT. Asked of Launch Services at the
// moment of the send, by bundle id, so a person who set it in Mail's
// preferences, or from Settings here, is never asked.
//
// ASKED ONCE, EVER, like the other one-time cards: the stamp is written when
// the card goes up, before any answer, so a crash with it on screen has still
// asked. "Not now" is an answer. Settings > Mail keeps the button for anyone
// who changes their mind.
//
// NEVER ON TOP OF ANOTHER CARD. A send while another one-time card is up does
// not stamp anything; the next send gets the chance instead.
//
// The system shows its own confirmation when an app asks to become the
// default (`setDefaultApplication`), so "Make default" is only the request.
// What Launch Services says afterwards is the answer, re-read rather than
// assumed.

import Foundation
import Observation

#if os(macOS)
    import AppKit
#endif

@MainActor
@Observable
final class DefaultMailApp {
    static let shared = DefaultMailApp()

    private nonisolated static let askedKey = "passband.defaultMail.asked"

    /// `PASSBAND_FORCE_DEFAULT_MAIL_NUDGE=1` puts the card up at launch without
    /// recording an ask, so it can be looked at without sending mail. Same
    /// posture as the other cards' overrides: a launch environment nobody sets
    /// by accident.
    nonisolated static let forced =
        ProcessInfo.processInfo.environment["PASSBAND_FORCE_DEFAULT_MAIL_NUDGE"] == "1"

    private let defaults = UserDefaults.standard

    /// Whether the card is up right now.
    var showingNudge = false

    /// What Launch Services last said: is Passband the app mailto: links open
    /// in. Refreshed on `refresh()`, which the Settings row calls on appear.
    private(set) var isDefault = false

    /// The last "Make default" failed or was declined at the system's prompt.
    private(set) var lastRequestFailed = false

    private init() {
        refresh()
        showingNudge = Self.forced
    }

    /// THE RULE, pure, so a headless suite can pin it. `sentFromPassband` is
    /// whether this moment follows a send that went out; nothing else is a
    /// moment to ask at.
    nonisolated static func earned(
        sentFromPassband: Bool, isDefault: Bool, asked: Bool, othersShowing: Bool
    ) -> Bool {
        sentFromPassband && !isDefault && !asked && !othersShowing
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

    /// A send from Passband just went out. Puts the card up if this is the
    /// moment, and stamps the ask when it does. The one entry point the store
    /// calls.
    func noteSent(othersShowing: Bool) {
        #if os(macOS)
            guard !FixtureMode.isEnabled, !showingNudge else { return }
            refresh()
            guard
                Self.earned(
                    sentFromPassband: true, isDefault: isDefault, asked: asked,
                    othersShowing: othersShowing)
            else { return }
            defaults.set(true, forKey: Self.askedKey)
            showingNudge = true
        #endif
    }

    /// Re-read the system's answer.
    func refresh() {
        #if os(macOS)
            let handler = Self.probeURL.flatMap { NSWorkspace.shared.urlForApplication(toOpen: $0) }
            let handlerId = handler.flatMap { Bundle(url: $0)?.bundleIdentifier }
            isDefault = Self.isPassband(
                handlerBundleId: handlerId, ownBundleId: Bundle.main.bundleIdentifier)
        #endif
    }

    /// Ask the system to make Passband the mailto: handler. The system shows
    /// its own confirmation; `isDefault` follows whatever it decided.
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

    private nonisolated static let probeURL = URL(string: "mailto:someone@example.com")
}
