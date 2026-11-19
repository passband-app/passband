// PASSBAND AS THE MAC'S DEFAULT MAIL APP: whether it is, the one time it asks
// macOS to make it so, and the Settings button that does the same.
//
// THE ASK IS THE SYSTEM'S ALONE. There is no card, sheet or step of ours: the
// first time an inbox is loaded over a verified connection, Passband decides,
// and a second later (so nothing lands mid-transition) asks macOS to make it
// the default mail app. macOS may show its own confirmation; that is the
// whole ask. One rule covers both roads to that moment:
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
// iPhone or agent ad, the share ask, the update card). If one is up at the
// decision, nothing is stamped and nothing waits behind it: the next launch
// tries again. The decision runs BEFORE What's New and the agent ad are
// offered at the first sync, and a launch that asks holds them, and the
// iPhone ad, until the next launch (`holdsCardsThisLaunch`): they never land
// beside the system's dialog.
//
// ONLY A BUNDLE THAT CLAIMS mailto: ASKS. Every build shares one bundle id
// and one defaults domain, and build.sh strips mailto: from all but release
// builds. A dev build asking would spend the one ask, for an app Launch
// Services cannot pick, on the very Mac the release will run on.
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

    /// Whether this bundle's Info.plist declares the mailto: scheme. Injected
    /// so a suite can stand in a plist.
    private let declaresMailto: () -> Bool

    /// This launch decided to ask. Two callers in the same second (the end of
    /// a first look and the first sync) make one request, and the other
    /// one-time cards hold until the next launch.
    private(set) var askedThisLaunch = false

    /// The other one-time cards (What's New, the agent ad, the iPhone ad)
    /// stay down for the rest of a launch that asked.
    static var holdsCardsThisLaunch: Bool { shared.askedThisLaunch }

    init(
        defaults: UserDefaults = .standard,
        forced: Bool = DefaultMailApp.forced,
        probe: @escaping () -> Bool = DefaultMailApp.systemSaysPassband,
        request: (() -> Void)? = nil,
        declaresMailto: @escaping () -> Bool = { DefaultMailApp.declaresMailto(Bundle.main.infoDictionary) }
    ) {
        self.defaults = defaults
        self.declaresMailto = declaresMailto
        self.probe = probe
        self.isForced = forced
        self.request = request ?? { DefaultMailApp.shared.makeDefault() }
        refresh()
    }

    /// THE RULE, pure: a bundle that claims mailto:, a verified connection,
    /// nothing else on screen, not already the default, and never asked (or
    /// the preview override).
    nonisolated static func shouldAsk(
        declaresMailto: Bool, verified: Bool, blocked: Bool, isDefault: Bool, asked: Bool,
        forced: Bool
    ) -> Bool {
        declaresMailto && verified && !blocked && !isDefault && (forced || !asked)
    }

    /// Whether an Info.plist's CFBundleURLTypes names the mailto: scheme.
    nonisolated static func declaresMailto(_ info: [String: Any]?) -> Bool {
        guard let types = info?["CFBundleURLTypes"] as? [[String: Any]] else { return false }
        return types.contains { type in
            (type["CFBundleURLSchemes"] as? [String] ?? []).contains { $0.lowercased() == "mailto" }
        }
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

    /// The inbox has loaded. DECIDES NOW, from `verified` and `blocked`
    /// (another one-time card up), and when the rule says ask: stamps, holds
    /// the other cards for the launch, and makes the request `after` the
    /// delay (nil: at once). Returns whether it will ask.
    @discardableResult
    func askIfNeverAsked(
        verified: Bool, blocked: Bool, after delay: Duration? = DefaultMailApp.askDelay
    ) -> Bool {
        guard !FixtureMode.isEnabled || isForced, !askedThisLaunch else { return false }
        refresh()
        guard
            Self.shouldAsk(
                declaresMailto: declaresMailto(), verified: verified, blocked: blocked,
                isDefault: isDefault, asked: asked, forced: isForced)
        else { return false }
        // The stamp BEFORE the request: a crash or a refusal never asks again.
        if !isForced { defaults.set(true, forKey: Self.askedKey) }
        askedThisLaunch = true
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
