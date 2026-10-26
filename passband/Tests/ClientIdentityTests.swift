import Foundation

@main
struct ClientIdentityTests {
    static func main() {
        // The shape the daemon parses (`ClientTag::parse` in squelch-core):
        // a platform from its closed set, a slash, dotted digits.
        // This suite runs on the Mac, so the platform half is not a guess.
        let value = ClientIdentity.headerValue(version: "0.0.12")
        precondition(value == "macos/0.0.12", "A release version is sent as <platform>/<version>")
        precondition(ClientIdentity.headerValue(version: "1.2") != nil)
        precondition(ClientIdentity.headerValue(version: "10.20.30.40") != nil)

        // Everything the daemon would drop is not sent in the first place.
        for refused in ["", "1", "1.2.3.4.5", "0.0.12-beta", "0..12", "0.0.123456", "0.0.12/x", "v0.0.12", "0.0.١٢"] {
            precondition(
                ClientIdentity.headerValue(version: refused) == nil,
                "\(refused) is not a version the daemon records")
        }
        precondition(ClientIdentity.headerValue(version: nil) == nil, "No bundle version, no header")

        // THE PATH THE APP TAKES: no argument, version read from the bundle.
        // test.sh links ClientIdentityTests.plist into this binary as its
        // Info.plist, so there is a version to read; without that, `current`
        // is nil and a stamp that did nothing at all would pass.
        precondition(
            ClientIdentity.current == "macos/0.0.12",
            "The version comes from the bundle's CFBundleShortVersionString")
        var request = URLRequest(url: URL(string: "https://daemon.example/client/stats")!)
        request.identifyClient()
        precondition(request.value(forHTTPHeaderField: "X-Passband-Client") == "macos/0.0.12")
        // The device zone rides along, as the IANA name the daemon validates.
        precondition(
            request.value(forHTTPHeaderField: "X-Passband-Timezone") == TimeZone.current.identifier,
            "The device zone is sent with the client tag")
        precondition(TimeZone(identifier: ClientIdentity.zone) != nil, "The zone is an IANA identifier")

        print("client-identity: ok")
    }
}
