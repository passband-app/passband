// APNs registration and daemon synchronization. The APNs token is capability
// material: it is held only in memory, sent in an authenticated JSON body, and
// never logged or placed in a URL.
//
// Every registration carries a TAG — this install's local account id. The
// daemon stores it against the device row and stamps it onto the pushes it
// sends there, which is the only thing that tells the Notification Service
// Extension WHICH mailbox an arriving `event_id` belongs to: event ids are
// per-daemon SQLite ints and two accounts hand out the same ones (the same
// collision `Notifier` prefixes its request identifiers against). The tag is a
// locally-minted UUID that names nothing about the mailbox, so the blind relay
// and APNs both stay blind while carrying it.
//
// It also carries the APNs ENVIRONMENT the token was minted in. A token is
// valid in exactly one of sandbox and production, and ONLY THE BUILD KNOWS
// WHICH: an Xcode or Simulator install and a TestFlight install of the same app
// hold tokens for different APNs hosts, and a daemon that guessed one host for
// every device pushed into the void for the rest (APNsEnvironment.swift). When
// the build cannot tell, the field is left out and the daemon's default stands.

import Foundation
import UIKit
import UserNotifications

@MainActor
final class PushRegistration {
    static let shared = PushRegistration()

    private let session = Sessions.ephemeral(
        timeout: 15, resource: 30,
        cachePolicy: .reloadIgnoringLocalCacheData, emptyHeaders: true)
    private var token: String?
    /// WHETHER A PUSH STANDS BEHIND AN ACCOUNT: its daemon accepted this
    /// device's token. Kept ACROSS LAUNCHES (a per-account bool, never the
    /// token), because the feed's first replay on a cold launch usually lands
    /// before this run's registration does, and that replay is exactly the
    /// backlog that must not banner again. Set by an accepted registration;
    /// cleared only by a definite refusal (4xx), a failed APNs registration,
    /// an unregister, or the account's removal (the key is account-scoped
    /// defaults). A timeout or being offline changes nothing: the daemon still
    /// has the device row.
    func covers(_ account: UUID) -> Bool {
        UserDefaults.standard.bool(forKey: Self.coveredKey(account))
    }

    static let coveredKeyBase = "passband.push.covered"
    private static func coveredKey(_ account: UUID) -> String {
        AccountIndex.scopedKey(coveredKeyBase, account)
    }

    private func setCovered(_ account: UUID, _ on: Bool) {
        if on {
            UserDefaults.standard.set(true, forKey: Self.coveredKey(account))
        } else {
            UserDefaults.standard.removeObject(forKey: Self.coveredKey(account))
        }
    }

    /// What a daemon said to a device call.
    private enum Answer { case accepted, refused, unknown }
    private var syncing = false
    private var needsAnotherSync = false

    /// `.badge` rides along because the app icon carries the For-your-eyes count
    /// (Model/Badge.swift). It is a separate grant from `.alert`, so a user can
    /// keep banners and refuse the number, and `setBadgeCount` then does nothing
    /// — which is the correct outcome, not a failure to handle.
    private static let wanted: UNAuthorizationOptions = [.alert, .sound, .badge]

    private init() {}

    func start() {
        Task { await registerAndSync() }
    }

    func registerAndSync() async {
        let center = UNUserNotificationCenter.current()
        let settings = await center.notificationSettings()
        switch settings.authorizationStatus {
        case .notDetermined:
            // NOT BEFORE THERE IS A MAILBOX. Launch and every foregrounding
            // call this, so on a fresh install the dialog used to land on top
            // of the cold open, asking to notify about mail from an account
            // that does not exist yet. Every path that connects one calls
            // back in here, which is where the question makes sense.
            guard !AccountManager.shared.accounts.isEmpty else { return }
            guard (try? await center.requestAuthorization(options: Self.wanted)) == true else {
                return
            }
        case .authorized:
            // BEST EFFORT, FOR THE INSTALLS THAT PREDATE THE BADGE. `.badge` was
            // not in the set this app asked for until the icon started carrying
            // the For-your-eyes count, and iOS grants what was asked for at the
            // time — so an install that said yes to alerts a month ago has no
            // badge permission and no prompt is coming. Asking again is silent
            // and instant on an already-authorized app (the system does not
            // re-prompt), so it costs nothing and may pick the option up.
            //
            // Not done for `.provisional` or `.ephemeral`: a full request from
            // those states DOES raise the dialog, and a permission prompt
            // nobody asked for, to add a number to an icon, is not a trade
            // worth making.
            _ = try? await center.requestAuthorization(options: Self.wanted)
        case .provisional, .ephemeral:
            break
        case .denied:
            return
        @unknown default:
            return
        }

        UIApplication.shared.registerForRemoteNotifications()
        if token != nil { await syncAllAccounts() }
    }

    func received(_ data: Data) async {
        token = data.map { String(format: "%02x", $0) }.joined()
        await syncAllAccounts()
    }

    func registrationFailed() {
        token = nil
        // No token, so no push reaches this phone for any account.
        for account in AccountManager.shared.accounts { setCovered(account.id, false) }
    }

    /// Tell one account's daemon to stop pushing to this device. Called on the
    /// way out of `AccountManager.remove`, while the credentials that authorize
    /// it are still in the keychain.
    ///
    /// BEST EFFORT, and deliberately not parked for retry the way a refused
    /// keychain delete is: what a missed unregister leaves behind is noise (a
    /// banner for a mailbox this install has forgotten, which the extension
    /// can no longer fetch content for and so renders generic), not a live
    /// capability. The daemon also drops the row by itself once APNs reports
    /// the token gone.
    func unregister(_ settings: ConnectionSettings) async {
        guard let token else { return }
        _ = await post("/client/devices/unregister", body: ["token": token], with: settings)
    }

    private func syncAllAccounts() async {
        guard let token else { return }
        if syncing {
            needsAnotherSync = true
            return
        }
        syncing = true
        defer { syncing = false }

        repeat {
            needsAnotherSync = false
            let ids = AccountManager.shared.accounts.map(\.id)
            for id in ids {
                guard let settings = await AccountManager.shared.settings(for: id) else { continue }
                await register(token: token, tag: id, with: settings)
            }
        } while needsAnotherSync
    }

    private func register(token: String, tag: UUID, with settings: ConnectionSettings) async {
        // Registration is idempotent and retried whenever the app foregrounds.
        var body = [
            "token": token,
            "platform": "ios",
            "tag": tag.uuidString,
        ]
        if let env = APNsEnvironment.current { body["environment"] = env.rawValue }
        switch await post("/client/devices", body: body, with: settings) {
        case .accepted: setCovered(tag, true)
        case .refused: setCovered(tag, false)
        case .unknown: break
        }
    }

    /// POST one authenticated JSON body to the human door. Neither the daemon
    /// URL nor the capability token reaches a log, here or in an error.
    /// 2xx accepted, 4xx refused, anything else (no answer, 5xx) unknown.
    @discardableResult
    private func post(_ path: String, body: [String: String], with settings: ConnectionSettings)
        async -> Answer
    {
        var base = settings.serverURL.trimmingCharacters(in: .whitespacesAndNewlines)
        while base.hasSuffix("/") { base.removeLast() }
        guard let url = URL(string: base + path),
            let encoded = try? JSONSerialization.data(withJSONObject: body)
        else { return .unknown }

        var request = URLRequest(url: url, timeoutInterval: 15)
        request.httpMethod = "POST"
        request.setValue("Bearer \(settings.apiToken)", forHTTPHeaderField: "Authorization")
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.identifyClient()
        request.httpBody = encoded
        guard let (_, response) = try? await session.data(for: request),
            let http = response as? HTTPURLResponse
        else { return .unknown }
        switch http.statusCode {
        case 200..<300: return .accepted
        case 400..<500: return .refused
        default: return .unknown
        }
    }
}
