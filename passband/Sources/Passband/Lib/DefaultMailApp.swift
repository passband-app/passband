// PASSBAND AS THE MAC'S DEFAULT MAIL APP: whether it is, the one time it asks
// macOS to make it so, and the Settings button that does the same.
//
// THE ASK IS THE SYSTEM'S ALONE. There is no card, sheet or step of ours: the
// first time an inbox is loaded over a verified connection, and a second
// later so nothing lands mid-transition, Passband asks macOS to make it the
// default mail app. macOS may show its own confirmation; that is the whole
// ask. One rule covers both roads to that moment:
//
//   * A NEW INSTALL, at the end of its first look (FirstLoad.conclude): the
//     inbox is on screen for the first time.
//   * AN INSTALL FROM BEFORE THIS VERSION, at the first sync of the launch
//     that brought it (RootView): never asked, so asked now.
//
// ONCE, EVER. The stamp is written BEFORE the request, so a crash or a
// refusal never asks again. Skipped when Passband is already the default.
//
// NEVER OVER ANOTHER ONE-TIME CARD (the first-load veil, What's New, the
// iPhone or agent ad, the share ask, the update card). If one is up when the
// second has passed, nothing is stamped and nothing waits behind it: the next
// launch tries again.
//
// Settings > Mail keeps the button for anyone who refused and changed their
// mind.

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

    /// `PASSBAND_FORCE_DEFAULT_MAIL_NUDGE=1` asks at the first verified inbox
    /// of this launch, past the stamp and without writing it, so the system's
    /// dialog can be looked at on any install. It still skips when Passband is
    /// already the default.
    nonisolated static let forced =
        ProcessInfo.processInfo.environment["PASSBAND_FORCE_DEFAULT_MAIL_NUDGE"] == "1"

    /// How long after the inbox is loaded before the system is asked.
    nonisolated static let askDelay: Duration = .seconds(1)

    private let defaults: UserDefaults
    /// Launch Services' answer: is Passband the app mailto: links open in.
    /// Injected so a suite can answer without the system.
    private let probe: () -> Bool
    /// Asks the system to make Passband the default. Injected so a suite can
    /// see the call, and what was stamped when it came.
    private let request: () -> Void
    private let isForced: Bool

    /// What Launch Services last said. Refreshed on `refresh()`, which the
    /// Settings row calls on appear and on every return to the app.
    private(set) var isDefault = false

    /// The last "Make default" failed or was declined.
    private(set) var lastRequestFailed = false

    /// An ask is waiting out its second, so a second caller in that window
    /// does not queue another.
    private var pending = false

    init(
        defaults: UserDefaults = .standard,
        forced: Bool = DefaultMailApp.forced,
        probe: @escaping () -> Bool = DefaultMailApp.systemSaysPassband,
        request: (() -> Void)? = nil
    ) {
        self.defaults = defaults
        self.probe = probe
        self.isForced = forced
        self.request = request ?? { DefaultMailApp.shared.makeDefault() }
        refresh()
    }

    /// THE RULE, pure: a verified connection, nothing else on screen, not
    /// already the default, and never asked (or the preview override).
    nonisolated static func shouldAsk(
        verified: Bool, blocked: Bool, isDefault: Bool, asked: Bool, forced: Bool
    ) -> Bool {
        verified && !blocked && !isDefault && (forced || !asked)
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

    /// The inbox has loaded. `after` the delay, reads `conditions` (whether
    /// the connection is verified, and whether another one-time card is up)
    /// and asks if the rule says so, stamping first. Nil `after` decides now.
    /// The completion says whether it asked.
    func askIfNeverAsked(
        after delay: Duration? = DefaultMailApp.askDelay,
        conditions: @escaping () -> (verified: Bool, blocked: Bool),
        done: ((Bool) -> Void)? = nil
    ) {
        guard !FixtureMode.isEnabled || isForced, !pending else {
            done?(false)
            return
        }
        guard let delay else {
            done?(decide(conditions()))
            return
        }
        pending = true
        Task { @MainActor in
            try? await Task.sleep(for: delay)
            self.pending = false
            let asked = self.decide(conditions())
            done?(asked)
        }
    }

    /// The rule over this install, and the stamp written before the request.
    private func decide(_ now: (verified: Bool, blocked: Bool)) -> Bool {
        refresh()
        guard
            Self.shouldAsk(
                verified: now.verified, blocked: now.blocked, isDefault: isDefault,
                asked: asked, forced: isForced)
        else { return false }
        if !isForced { defaults.set(true, forKey: Self.askedKey) }
        request()
        return true
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
