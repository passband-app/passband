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

    /// Whether the fixture inbox is on screen right now. Read from every
    /// layer the fixture transport has to intercept: the API client's actor,
    /// view bodies, the poller. So it is a lock, not a main-actor property.
    static var isEnabled: Bool { state.withLock { $0 } }

    static func setEnabled(_ enabled: Bool) { state.withLock { $0 = enabled } }
}
