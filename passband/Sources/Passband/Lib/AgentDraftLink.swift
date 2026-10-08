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

/// The stream frame.
///
/// Decoded only after a frame has failed to decode as a notification event,
/// so the two can never be confused for each other. `at` is when the daemon
/// relayed the request (Unix seconds); a daemon that predates it sends none.
struct AgentOpenSignal: Decodable, Equatable, Sendable {
    let open_draft: Int
    var at: Int?

    /// How old a request may be and still be acted on. The connection to a
    /// sleeping Mac stays open on the daemon's side, so a request made while
    /// the lid was shut arrives when it opens: "show this now" from an hour
    /// ago is a window thrown at somebody for no reason they remember. Wide
    /// enough to cover two machines' clocks disagreeing; a request outside it
    /// is dropped, never queued.
    static let freshFor = 60

    static func parse(_ data: String) -> AgentOpenSignal? {
        guard let bytes = data.data(using: .utf8),
            let signal = try? JSONDecoder().decode(AgentOpenSignal.self, from: bytes),
            signal.open_draft > 0
        else { return nil }
        return signal
    }

    /// Whether to act on it, judged at `now` (Unix seconds). A signal with no
    /// date is taken as fresh: it came from a daemon too old to say.
    func isFresh(now: Int) -> Bool {
        guard let at else { return true }
        return abs(now - at) <= Self.freshFor
    }
}

/// A `passband://draft/<id>?mailbox=<address>&t=<created>` link.
///
/// STRICT, like `PairLink`: this arrives from whatever opened the URL, which
/// can be any web page or any local process. Exactly the shape the daemon
/// mints (squelch-core `draft_link`) parses and nothing looser: no user, port
/// or fragment, one path segment that is a plain positive integer with no
/// leading zero, and a creation time.
///
/// `created` IS WHAT BINDS A LINK TO ONE DRAFT. Draft ids are small row ids
/// that are reused once a draft is sent or discarded, so an id alone would let
/// an old link open a different draft, and let anything walk `draft/1..N`.
/// The app opens a link only for a draft created at exactly this second. The
/// mailbox is a hint for choosing between accounts, compared and never shown.
struct AgentDraftLink: Equatable, Sendable {
    var draftId: Int
    /// Unix seconds; the draft's `created_at`.
    var created: Int
    /// Lowercased, or nil when the link named none.
    var mailbox: String?

    init?(_ url: URL) {
        guard url.scheme?.lowercased() == "passband", url.host?.lowercased() == "draft",
            url.user == nil, url.password == nil, url.port == nil, url.fragment == nil
        else { return nil }
        // The path AS WRITTEN, not decoded: an id is digits, and a link that
        // spells them any other way is not one this app minted.
        let path = url.path(percentEncoded: true)
        guard path.hasPrefix("/"), let id = Self.plainNumber(path.dropFirst(), maxDigits: 12) else { return nil }
        let query = URLComponents(url: url, resolvingAgainstBaseURL: false)?.queryItems ?? []
        func only(_ name: String) -> String? {
            let found = query.filter { $0.name == name }
            return found.count == 1 ? found[0].value : nil
        }
        guard let stamp = only("t"), let created = Self.plainNumber(Substring(stamp), maxDigits: 11) else {
            return nil
        }
        draftId = id
        self.created = created
        let named = only("mailbox")?.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        if let named, !named.isEmpty, named.count <= 320 {
            mailbox = named
        } else {
            mailbox = nil
        }
    }

    /// A positive integer written the one plain way: ASCII digits, no sign,
    /// no leading zero, nothing else.
    private static func plainNumber(_ text: Substring, maxDigits: Int) -> Int? {
        guard !text.isEmpty, text.count <= maxDigits, text.first != "0",
            text.allSatisfy({ $0.isASCII && $0.isNumber }), let n = Int(text), n > 0
        else { return nil }
        return n
    }

    /// Whether `createdAt` (a draft's RFC 3339 `created_at`) is this link's
    /// second. A draft whose time cannot be read is never a match.
    func names(draftCreatedAt createdAt: String) -> Bool {
        let withFraction = ISO8601DateFormatter()
        withFraction.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        guard let date = withFraction.date(from: createdAt) ?? ISO8601DateFormatter().date(from: createdAt)
        else { return false }
        return Int(date.timeIntervalSince1970.rounded(.down)) == created
    }
}
