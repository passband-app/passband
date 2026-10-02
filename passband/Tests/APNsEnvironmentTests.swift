// Which APNs environment a build declares when it registers its device token.
// A wrong answer here is invisible everywhere but a lock screen that never
// lights up: the daemon pushes to the host the client named, APNs refuses a
// token from the other environment, and nothing on the phone says so. So every
// branch of the signing rule is asserted with a synthetic profile rather than
// trusted to whatever device happens to be on the desk.

import Foundation

@main
@MainActor
struct APNsEnvironmentTests {
    static var failures = 0
    static var checks = 0

    static func main() {
        simulator()
        storeSigned()
        embeddedProfile()
        unreadableProfile()

        if failures > 0 {
            print("FAILED: \(failures) of \(checks) checks")
            exit(1)
        }
        print("ok: \(checks) checks passed")
    }

    // MARK: - cases

    /// The Simulator is sandbox whatever is or is not in the bundle.
    static func simulator() {
        equal(
            APNsEnvironment.resolve(isSimulator: true, provisioningProfile: nil), .sandbox,
            "simulator, no profile")
        equal(
            APNsEnvironment.resolve(
                isSimulator: true, provisioningProfile: profile(aps: "production")),
            .sandbox, "simulator ignores a production profile")
        equal(
            APNsEnvironment.resolve(isSimulator: true, provisioningProfile: Data([0, 1, 2])),
            .sandbox, "simulator ignores junk")
    }

    /// No profile on hardware: App Store processing stripped it, so the token
    /// is a production one.
    static func storeSigned() {
        equal(
            APNsEnvironment.resolve(isSimulator: false, provisioningProfile: nil), .production,
            "device without a profile is store-signed")
    }

    /// The plist is cut out of a signed blob, junk on both sides.
    static func embeddedProfile() {
        equal(
            APNsEnvironment.resolve(
                isSimulator: false, provisioningProfile: profile(aps: "development")),
            .sandbox, "development profile")
        equal(
            APNsEnvironment.resolve(
                isSimulator: false, provisioningProfile: profile(aps: "production")),
            .production, "production (ad hoc / enterprise) profile")
        // Some profiles start at `<plist` with no XML declaration.
        equal(
            APNsEnvironment.resolve(
                isSimulator: false,
                provisioningProfile: profile(aps: "development", declaration: false)),
            .sandbox, "plist without an xml declaration")
    }

    /// Anything the rule cannot read says nothing, never a guess.
    static func unreadableProfile() {
        equal(
            APNsEnvironment.resolve(isSimulator: false, provisioningProfile: profile(aps: nil)),
            nil, "profile without aps-environment")
        equal(
            APNsEnvironment.resolve(
                isSimulator: false, provisioningProfile: profile(aps: "staging")),
            nil, "unknown aps-environment value")
        equal(
            APNsEnvironment.resolve(
                isSimulator: false, provisioningProfile: Data([0x30, 0x82, 0xFF, 0x00, 0x13])),
            nil, "binary junk with no plist")
        equal(
            APNsEnvironment.resolve(
                isSimulator: false,
                provisioningProfile: wrap("<?xml version=\"1.0\"?><plist><dict><key>Ent")),
            nil, "truncated plist with no close tag")
        equal(
            APNsEnvironment.resolve(
                isSimulator: false,
                provisioningProfile: wrap(
                    "<?xml version=\"1.0\"?><plist version=\"1.0\"><dict><key>x</dict></plist>")),
            nil, "malformed plist")
        equal(
            APNsEnvironment.resolve(isSimulator: false, provisioningProfile: Data()),
            nil, "empty file")
    }

    // MARK: - fixtures

    /// A profile shaped like the real thing: CMS bytes, the plist, more CMS.
    static func profile(aps: String?, declaration: Bool = true) -> Data {
        let apsEntry = aps.map { "<key>aps-environment</key><string>\($0)</string>" } ?? ""
        let xml = """
            \(declaration ? "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n" : "")\
            <!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" \
            "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
            <plist version="1.0">
            <dict>
              <key>AppIDName</key><string>Passband</string>
              <key>Entitlements</key>
              <dict>
                \(apsEntry)
                <key>application-identifier</key><string>AAT39BQ9LV.app.passband.client.ios</string>
              </dict>
            </dict>
            </plist>
            """
        return wrap(xml)
    }

    static func wrap(_ xml: String) -> Data {
        var data = Data([0x30, 0x80, 0x06, 0x09, 0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x07, 0x02])
        data.append(Data(xml.utf8))
        data.append(Data([0x00, 0x00, 0xA0, 0x82, 0x0B, 0xFF, 0x3C, 0x2F]))
        return data
    }

    // MARK: - harness

    static func equal<T: Equatable>(
        _ got: T, _ want: T, _ label: String, line: Int = #line
    ) {
        checks += 1
        if got != want {
            failures += 1
            print("FAIL (line \(line)): \(label)\n  want: \(want)\n   got: \(got)")
        }
    }
}
