// What this app tells ITS OWN DAEMON about itself, and nobody else: a platform
// and a version, as `X-Passband-Client: macos/0.0.12`. The daemon keeps the
// last one each device token presented, which is how an operator can tell a
// mailbox whose Mac is three releases behind from one nobody opens.
//
// A HEADER ON THE REQUEST, never a default on the session. `Sessions` builds
// the sessions that fetch images an email chose, and anything set there goes
// to whoever the sender pointed us at. So this is stamped by hand, by the few
// builders that address the daemon, and a request that does not call
// `identifyClient()` says nothing.
//
// The notification extension does not call it, on purpose: it presents the
// same token every time a push lands, with no app open, and "last connected"
// is meant to be the app.
//
// The device's time zone goes with it (`X-Passband-Timezone: America/Los_Angeles`).
// The daemon's agents read "now" in the zone of the app that was used last, so
// "due tomorrow" means the user's tomorrow and travel needs no setting.
//
// Foundation-only so a test suite can compile it alone.

import Foundation

enum ClientIdentity {
    /// The daemon's name for it is `store::CLIENT_HEADER` in squelch-core.
    static let headerField = "X-Passband-Client"

    /// The daemon's name for it is `owner_clock::ZONE_HEADER` in squelch-core.
    static let zoneHeaderField = "X-Passband-Timezone"

    /// The device's IANA zone, read per request so a phone that changed zones
    /// says so on its next request rather than on its next launch.
    static var zone: String { TimeZone.current.identifier }

    /// One of the daemon's closed set (`store::ClientPlatform`). A value it
    /// does not know is not an error there; the request is just not recorded.
    static var platform: String {
        #if os(macOS)
            "macos"
        #else
            "ios"
        #endif
    }

    /// `<platform>/<version>`, or nil when there is no version to send: a test
    /// binary has no bundle, and a version the daemon would refuse (it takes
    /// two to four dot-separated runs of one to five digits, nothing else) is
    /// better left unsaid than sent to be dropped on every request.
    static func headerValue(version: String?) -> String? {
        guard let version else { return nil }
        let parts = version.split(separator: ".", omittingEmptySubsequences: false)
        guard (2...4).contains(parts.count),
            parts.allSatisfy({ (1...5).contains($0.utf8.count) && $0.utf8.allSatisfy { (48...57).contains($0) } })
        else { return nil }
        return "\(platform)/\(version)"
    }

    /// This copy of the app. Read from the bundle, like `WhatsNew`, so it is
    /// what shipped rather than what passband/VERSION said at some point.
    static let current: String? = headerValue(
        version: Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String)
}

extension URLRequest {
    /// Name this app to the daemon the request is addressed to. Only for
    /// requests to the user's own daemon; see the top of this file.
    mutating func identifyClient() {
        guard let value = ClientIdentity.current else { return }
        setValue(value, forHTTPHeaderField: ClientIdentity.headerField)
        // Only with the tag: the daemon records a zone only for a request that
        // named its app.
        setValue(ClientIdentity.zone, forHTTPHeaderField: ClientIdentity.zoneHeaderField)
    }
}
