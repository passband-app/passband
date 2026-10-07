// Two ways an agent's draft is asked onto the screen, and the parsing for
// both. Pure, Foundation-only, so the test suite compiles it alone.
//
// 1. A SIGNAL on the account's event stream: the agent called `open_draft`
//    and the daemon is relaying it to this Mac (`{"open_draft": <id>}`, a
//    frame that is not a notification event and carries no cursor).
// 2. A LINK, `passband://draft/<id>?mailbox=<address>`, which the agent hands
//    its human when no Mac was listening.
//
// Both name a draft by one daemon's row id and nothing else. Neither can
// carry content, and neither is trusted: what is opened is whatever draft the
// daemon holds under that id, and only if the daemon says an agent wrote it.

import Foundation

/// The stream frame. Decoded only after a frame has failed to decode as a
/// notification event, so the two can never be confused for each other.
struct AgentOpenSignal: Decodable, Equatable, Sendable {
    let open_draft: Int

    static func parse(_ data: String) -> AgentOpenSignal? {
        guard let bytes = data.data(using: .utf8),
            let signal = try? JSONDecoder().decode(AgentOpenSignal.self, from: bytes),
            signal.open_draft > 0
        else { return nil }
        return signal
    }
}

/// A `passband://draft/<id>?mailbox=<address>` link.
///
/// STRICT, like `PairLink`: this arrives from whatever opened the URL, which
/// can be any web page. The id must be a plain positive integer and nothing
/// may follow it in the path. The mailbox is a hint for choosing between
/// accounts, compared and never displayed as markup; a link without one still
/// parses.
struct AgentDraftLink: Equatable, Sendable {
    var draftId: Int
    /// Lowercased, or nil when the link named none.
    var mailbox: String?

    init?(_ url: URL) {
        guard url.scheme?.lowercased() == "passband", url.host?.lowercased() == "draft" else { return nil }
        // The path AS WRITTEN, not decoded: an id is digits, and a link that
        // spells them any other way is not one this app minted.
        let parts = url.path(percentEncoded: true).split(separator: "/", omittingEmptySubsequences: true)
        guard parts.count == 1, parts[0].allSatisfy(\.isASCII), parts[0].allSatisfy(\.isNumber),
            parts[0].count <= 12, let id = Int(parts[0]), id > 0
        else { return nil }
        draftId = id
        let named = URLComponents(url: url, resolvingAgainstBaseURL: false)?
            .queryItems?.first { $0.name == "mailbox" }?.value?
            .trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        if let named, !named.isEmpty, named.count <= 320 {
            mailbox = named
        } else {
            mailbox = nil
        }
    }
}
