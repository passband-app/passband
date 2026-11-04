import Foundation
import os

/// The fixture inbox is a MODE of the one app, not a second process. While it
/// is on, the API client answers from `FixtureAPI`, the board and reader draw
/// `FixtureMail`, and the surfaces that would reach past the transport (favicon
/// and hero fetches, account photos, the agent door probe, calendar writes)
/// hold still. It exists for the phone's debug screenshot harness
/// (`PassbandiOS/Lib/FixtureStage.swift`, `--fixture-inbox`); nothing a customer
/// can reach turns it on.
///
/// Only `AppStore.enterFixtures` flips the flag, and it fences the store's
/// epoch first, so a fixture answer can never land in a live read model.
enum FixtureMode {
    private static let state = OSAllocatedUnfairLock(initialState: false)

    /// THE USAGE PAGE ON A STAGE, for a demo or a screenshot with no daemon
    /// (`--fixture-usage`, the Mac's one fixture launch): the app boots
    /// straight into the fixture transport with the Understanding fixture
    /// staged and the Usage page on screen, and does not take focus.
    /// Compiled out of a shipped build, so no customer launch can stage it.
    static let stagesUsage: Bool = {
        #if PASSBAND_RELEASE
            return false
        #else
            return ProcessInfo.processInfo.arguments.contains("--fixture-usage")
        #endif
    }()

    /// A staged detail on that page, for a screenshot that cannot hover or
    /// click: `--fixture-usage-hover=<addr>` holds the People chart's readout
    /// on one sender, `--fixture-usage-tab=fading` opens Your people on its
    /// second tab, `--fixture-usage-scroll=knows` scrolls to that card
    /// (`your-people`, `knows`, `none`). Nil unless the page is staged.
    ///
    /// ONE TOKEN EACH, `name=value`, never `name value`: AppKit reads a bare
    /// argument after the flags as a file to open, and an app launched to
    /// open a file does not open its window.
    static func stageValue(_ name: String) -> String? {
        guard stagesUsage else { return nil }
        let prefix = "--fixture-usage-" + name + "="
        return ProcessInfo.processInfo.arguments.first { $0.hasPrefix(prefix) }
            .map { String($0.dropFirst(prefix.count)) }
    }

    /// Whether the fixture inbox is on screen right now. Read from every
    /// layer the fixture transport has to intercept: the API client's actor,
    /// view bodies, the poller. So it is a lock, not a main-actor property.
    static var isEnabled: Bool { state.withLock { $0 } }

    static func setEnabled(_ enabled: Bool) { state.withLock { $0 = enabled } }
}
