// THE MAC'S APNs TOKEN, for one kind of push: "a new version is out", which
// the daemon sends when this Mac has not picked a release up on its own
// (squelch-core/src/release_feed.rs). macOS draws a remote push even while
// Passband is QUIT, which is the whole point: the updater's own banner only
// exists while the app runs.
//
// Not the phone's PushRegistration, on purpose. That one marks an account as
// covered by push, and the feed then leaves its banners to the push. A Mac is
// never pushed mail (the daemon leaves `macos` rows out of every mail fan-out),
// so nothing here may tell the feed to stand down.
//
// ONLY A BUILD WITH THE PROFILE REGISTERS. Push is a profile-gated entitlement:
// build.sh adds it, with the Developer ID provisioning profile, when the
// release has one (MAC_PUSH_PROFILE). A build without the profile has no
// entitlement, would only ever be refused a token, and does not ask.
//
// The token is capability material: held in memory, sent in an authenticated
// body, never logged and never in a URL. The tag is this install's local
// account id, as on the phone, so the daemon's push can name the mailbox.

#if os(macOS)
    import AppKit
    import Foundation

    @MainActor
    final class MacPush {
        static let shared = MacPush()

        private let session = Sessions.ephemeral(
            timeout: 15, resource: 30,
            cachePolicy: .reloadIgnoringLocalCacheData, emptyHeaders: true)
        private var token: String?
        private var lastSync: Date?
        private var syncing = false
        /// A sync was asked for while one ran (an account added mid-sync).
        private var again = false

        /// How often an activation re-sends the token. Registration is
        /// idempotent and the daemon keeps the row; this only refreshes it.
        private static let resyncAfter: TimeInterval = 6 * 3600

        private init() {}

        /// The APNs environment the embedded profile grants, or nil when this
        /// build has none (and so cannot hold a token).
        nonisolated static let environment: String? = {
            let url = Bundle.main.bundleURL.appendingPathComponent("Contents/embedded.provisionprofile")
            guard let data = try? Data(contentsOf: url) else { return nil }
            return apsEnvironment(in: data)
        }()

        /// `com.apple.developer.aps-environment` out of a provisioning profile:
        /// a CMS blob with the XML plist inside it verbatim, cut out by its own
        /// markers. `development` is APNs' sandbox.
        nonisolated static func apsEnvironment(in profile: Data) -> String? {
            guard let start = profile.range(of: Data("<?xml".utf8)),
                let end = profile.range(of: Data("</plist>".utf8), in: start.lowerBound..<profile.endIndex),
                let plist = try? PropertyListSerialization.propertyList(
                    from: profile[start.lowerBound..<end.upperBound], format: nil) as? [String: Any],
                let entitlements = plist["Entitlements"] as? [String: Any],
                let aps = entitlements["com.apple.developer.aps-environment"] as? String
            else { return nil }
            switch aps {
            case "production": return "production"
            case "development": return "sandbox"
            default: return nil
            }
        }

        /// Launch, and every activation after the throttle: ask AppKit for the
        /// token, which answers through the app delegate.
        func start() {
            guard Self.environment != nil else { return }
            if let lastSync, Date().timeIntervalSince(lastSync) < Self.resyncAfter { return }
            NSApp.registerForRemoteNotifications()
        }

        func received(_ data: Data) async {
            token = data.map { String(format: "%02x", $0) }.joined()
            await syncAll()
        }

        /// An account was added: tell its daemon now, not at the next resync.
        func accountAdded() async {
            await syncAll()
        }

        /// Tell one account's daemon to stop pushing here, on the way out of
        /// `AccountManager.remove`. Best effort, as on the phone: a missed one
        /// leaves a release banner for a forgotten mailbox, nothing more, and
        /// APNs retires the token by itself if the app is deleted.
        func unregister(_ settings: ConnectionSettings) async {
            guard let token else { return }
            _ = await post("/client/devices/unregister", body: ["token": token], with: settings)
        }

        private func syncAll() async {
            guard let token, let environment = Self.environment else { return }
            guard !syncing else {
                again = true
                return
            }
            syncing = true
            defer { syncing = false }
            repeat {
                again = false
                await syncOnce(token: token, environment: environment)
            } while again
        }

        /// One pass over every account. The throttle starts only when every
        /// daemon took the token: one that was down is asked again on the
        /// next activation, not six hours later.
        private func syncOnce(token: String, environment: String) async {
            var allTook = true
            for account in AccountManager.shared.accounts {
                guard let settings = await AccountManager.shared.settings(for: account.id) else {
                    allTook = false
                    continue
                }
                let took = await post(
                    "/client/devices",
                    body: [
                        "token": token,
                        "platform": "macos",
                        "tag": account.id.uuidString,
                        "environment": environment,
                    ], with: settings)
                allTook = allTook && took
            }
            lastSync = allTook ? Date() : nil
        }

        /// POST one authenticated JSON body to the human door. Neither the
        /// daemon URL nor the bearer reaches a log. True on a 2xx.
        private func post(_ path: String, body: [String: String], with settings: ConnectionSettings)
            async -> Bool
        {
            var base = settings.serverURL.trimmingCharacters(in: .whitespacesAndNewlines)
            while base.hasSuffix("/") { base.removeLast() }
            guard let url = URL(string: base + path),
                let encoded = try? JSONSerialization.data(withJSONObject: body)
            else { return false }
            var request = URLRequest(url: url, timeoutInterval: 15)
            request.httpMethod = "POST"
            request.setValue("Bearer \(settings.apiToken)", forHTTPHeaderField: "Authorization")
            request.setValue("application/json", forHTTPHeaderField: "Accept")
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
            request.identifyClient()
            request.httpBody = encoded
            guard let (_, response) = try? await session.data(for: request),
                let http = response as? HTTPURLResponse
            else { return false }
            return (200..<300).contains(http.statusCode)
        }
    }
#endif
