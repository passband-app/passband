// OPEN AT LOGIN. Passband is a notifier, so it is only useful while it runs:
// new-mail banners and the update banner both come from this process, and a
// Mac that restarted with Passband quit hears nothing until somebody opens it.
//
// ON BY DEFAULT, adopted once per install. The first release launch registers
// the app as a login item and stamps that it did; after that the switch in
// Settings (and System Settings, General, Login Items, which writes the same
// registration) is the human's, and turning it off stays off. An install from
// before this existed is adopted on its first launch of the build that added
// it, which is the point: existing installs are the ones that restart quietly.
//
// DEV BUILDS NEVER REGISTER. A login item names a bundle on disk, and a dev
// build lives in a worktree that gets deleted; registering it would leave a
// login item pointing at nothing, or worse, at a stale branch. Same posture as
// the updater (`Updater.init`): PASSBAND_LOGIN_ITEM opts a dev build in on
// purpose, for working on this file.

#if os(macOS)
    import Foundation
    import Observation
    import ServiceManagement

    @MainActor
    @Observable
    final class LoginItem {
        static let shared = LoginItem()

        private nonisolated static let adoptedKey = "passband.loginItem.adopted"

        /// Whether this build may touch the registration at all.
        nonisolated static var available: Bool {
            #if PASSBAND_RELEASE
                true
            #else
                ProcessInfo.processInfo.environment["PASSBAND_LOGIN_ITEM"] != nil
            #endif
        }

        /// The system's answer, re-read on `refresh()`. Not cached across
        /// launches: System Settings can change it while Passband is quit.
        private(set) var status: SMAppService.Status = .notRegistered
        /// Why the last switch flip failed, until the next one.
        private(set) var error: String?

        /// On, as far as the switch should show it. `requiresApproval` counts:
        /// the registration exists and only the human's OK in System Settings
        /// is missing, which `needsApproval` says out loud.
        var enabled: Bool { status == .enabled || status == .requiresApproval }
        var needsApproval: Bool { status == .requiresApproval }

        private init() { refresh() }

        func refresh() {
            status = SMAppService.mainApp.status
        }

        /// Launch-time default: register once, ever, per install.
        func adoptDefault() {
            guard Self.available else { return }
            let defaults = UserDefaults.standard
            guard !defaults.bool(forKey: Self.adoptedKey) else { return refresh() }
            // Stamped before trying, so a registration the system refuses is
            // not retried every launch. The switch is still there to try again.
            defaults.set(true, forKey: Self.adoptedKey)
            if SMAppService.mainApp.status != .enabled {
                try? SMAppService.mainApp.register()
            }
            refresh()
        }

        /// The Settings switch.
        func set(_ on: Bool) {
            guard Self.available else { return }
            error = nil
            do {
                if on {
                    try SMAppService.mainApp.register()
                } else {
                    try SMAppService.mainApp.unregister()
                }
            } catch {
                self.error =
                    on
                    ? "macOS did not allow it. Turn it on in System Settings, General, Login Items."
                    : "macOS did not allow it. Turn it off in System Settings, General, Login Items."
            }
            refresh()
        }

        func openSystemSettings() {
            SMAppService.openSystemSettingsLoginItems()
        }
    }
#endif
