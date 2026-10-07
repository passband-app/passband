import Foundation

/// Whether a server is a hosted Passband mailbox rather than a self-hosted
/// daemon. One rule, shared: the connect screen uses it to put a returning
/// person back on the right form, and the Agents pane uses it so a hosted
/// person is never told to edit a daemon's environment they do not have.
///
/// Every hosted tenant lives at `<label>.passband.email` (squelch-warden
/// renders one host per mailbox under that base domain), so the host is the
/// whole answer. The app holds no other hosted flag: the human door is the
/// same on both, by design.
enum HostedServer {
    static let baseDomain = "passband.email"

    /// `host` as the account index or a URL carries it (no scheme). Hosts are
    /// case-insensitive, so the comparison is too. The match stops at a label
    /// boundary: `notpassband.email` is somebody else's domain.
    static func isHosted(host: String) -> Bool {
        let host = host.lowercased()
        return host == baseDomain || host.hasSuffix("." + baseDomain)
    }

    /// The same question of a full URL such as the server address.
    static func isHosted(url: String) -> Bool {
        guard let host = URLComponents(string: url)?.host else { return false }
        return isHosted(host: host)
    }
}
