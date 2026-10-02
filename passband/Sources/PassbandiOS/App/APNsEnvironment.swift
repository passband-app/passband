// Which APNs environment this build's device token belongs to. APNs keeps two,
// and a token minted in one is SILENTLY USELESS in the other: an Xcode build or
// the Simulator gets a sandbox token, a TestFlight or App Store build gets a
// production one. The daemon cannot tell them apart by looking, so the app says
// which when it registers (PushRegistration).
//
// The answer comes from how the build was SIGNED, never from `#if DEBUG`. The
// configuration only says what a build was meant to be; the profile it was
// installed under says what APNs will actually hand it, and guessing from the
// configuration is exactly the mistake that sent every push to the wrong place.
//
// Foundation only, and kept that way so test.sh can compile it on the Mac.
// App target ONLY: `current` reads `Bundle.main`, which inside the
// notification service extension is the extension, not the app.

import Foundation

enum APNsEnvironment: String {
    case production
    case sandbox

    /// The environment this build's device token belongs to, or nil when it
    /// cannot be determined. Computed once: neither input changes while the
    /// process lives.
    static var current: APNsEnvironment? { cached }

    private static let cached: APNsEnvironment? = {
        #if targetEnvironment(simulator)
            let isSimulator = true
        #else
            let isSimulator = false
        #endif
        let profile = Bundle.main.url(forResource: "embedded", withExtension: "mobileprovision")
            .flatMap { try? Data(contentsOf: $0) }
        return resolve(isSimulator: isSimulator, provisioningProfile: profile)
    }()

    /// Pure and testable: everything `current` reads is passed in.
    static func resolve(isSimulator: Bool, provisioningProfile: Data?) -> APNsEnvironment? {
        // The Simulator has no profile, and its tokens only work against sandbox.
        if isSimulator { return .sandbox }
        // NO PROFILE ON HARDWARE MEANS A STORE-SIGNED BUILD. App Store
        // processing strips `embedded.mobileprovision` from TestFlight and App
        // Store builds; development, ad hoc and enterprise builds keep theirs.
        guard let provisioningProfile else { return .production }
        // A profile we cannot read answers nil, not a guess: the daemon then
        // applies its own default rather than ours.
        switch apsEnvironment(in: provisioningProfile) {
        case "development": return .sandbox
        case "production": return .production
        default: return nil
        }
    }

    /// `Entitlements` -> `aps-environment` from a provisioning profile. The
    /// file is a CMS-signed blob with the XML plist sitting in it verbatim, so
    /// the plist is cut out by its own markers rather than by unwrapping the
    /// signature.
    private static func apsEnvironment(in profile: Data) -> String? {
        guard
            let start = profile.range(of: Data("<?xml".utf8))
                ?? profile.range(of: Data("<plist".utf8)),
            let end = profile.range(
                of: Data("</plist>".utf8), in: start.lowerBound..<profile.endIndex)
        else { return nil }
        let xml = profile.subdata(in: start.lowerBound..<end.upperBound)
        guard
            let plist = try? PropertyListSerialization.propertyList(from: xml, format: nil),
            let root = plist as? [String: Any],
            let entitlements = root["Entitlements"] as? [String: Any]
        else { return nil }
        return entitlements["aps-environment"] as? String
    }
}
