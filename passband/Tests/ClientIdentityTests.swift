import Foundation

@main
struct ClientIdentityTests {
    static func main() {
        // The shape the daemon parses (`ClientTag::parse` in squelch-core):
        // a platform from its closed set, a slash, dotted digits.
        let value = ClientIdentity.headerValue(version: "0.0.12")
        precondition(
            value == "macos/0.0.12" || value == "ios/0.0.12",
            "A release version is sent as <platform>/<version>")
        precondition(ClientIdentity.headerValue(version: "1.2") != nil)
        precondition(ClientIdentity.headerValue(version: "10.20.30.40") != nil)

        // Everything the daemon would drop is not sent in the first place.
        for refused in ["", "1", "1.2.3.4.5", "0.0.12-beta", "0..12", "0.0.123456", "0.0.12/x", "v0.0.12", "0.0.١٢"] {
            precondition(
                ClientIdentity.headerValue(version: refused) == nil,
                "\(refused) is not a version the daemon records")
        }
        precondition(ClientIdentity.headerValue(version: nil) == nil, "No bundle version, no header")

        // A test binary has no bundle version, so the stamp is a no-op here,
        // and that is the property: a request is never sent a made-up one.
        var request = URLRequest(url: URL(string: "https://daemon.example/client/stats")!)
        request.identifyClient()
        precondition(
            request.value(forHTTPHeaderField: ClientIdentity.headerField) == ClientIdentity.current)

        print("client-identity: ok")
    }
}
