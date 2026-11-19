// A mailto: link clicked inside a message, or handed over by another app
// while Passband is the default mail app, read into what a new composer
// opens with (RFC 6068).
//
// The link comes from the MAIL (or a web page), so it is untrusted: it only ever fills in a
// composer the person reads and sends themselves. Only the five fields a
// composer shows are taken (to, cc, bcc, subject, body); any other header a
// link names (in-reply-to, from, attach, ...) is dropped rather than carried
// somewhere the person cannot see it. Characters that could make the card or
// the fields display something other than what is there (controls, bidi
// overrides, zero-widths) are taken out, and an address holding one is
// refused outright.

import Foundation

struct Mailto: Equatable, Sendable {
    var to: [String] = []
    var cc: [String] = []
    var bcc: [String] = []
    var subject: String = ""
    var body: String = ""

    /// A link is a suggestion, not a document: past these it is cut, never
    /// refused. The href cap bounds the parse itself, on the main thread.
    static let hrefLimit = 64 * 1024
    static let recipientLimit = 50
    static let subjectLimit = 500
    static let bodyLimit = 10_000

    /// True when `url` is a mailto: link, whatever its case.
    static func isMailto(_ url: String) -> Bool {
        url.range(of: "mailto:", options: [.caseInsensitive, .anchored]) != nil
    }

    /// The link read into fields, or nil for anything that is not a mailto:
    /// link, is too long to be one, or names only addresses that were all
    /// refused. A bare `mailto:` is a blank message. A link with a
    /// subject or body and nobody to write to is fine (RFC 6068 allows it):
    /// the person picks the recipient.
    init?(_ url: String) {
        guard Self.isMailto(url), url.utf8.count <= Self.hrefLimit else { return nil }
        var rest = url.dropFirst("mailto:".count)
        // A fragment is not part of any field.
        if let hash = rest.firstIndex(of: "#") { rest = rest[..<hash] }
        let (path, query) = Self.split(rest)
        var to = Self.addresses(path)
        var cc: [String] = []
        var bcc: [String] = []
        for pair in query.split(separator: "&", omittingEmptySubsequences: true) {
            let parts = pair.split(separator: "=", maxSplits: 1, omittingEmptySubsequences: false)
            let name = Self.decode(parts[0]).lowercased()
            let raw = parts.count > 1 ? parts[1] : ""
            switch name {
            // Raw, not decoded: `addresses` decodes each one ONCE, so an
            // encoded comma stays inside its address instead of splitting.
            case "to": to += Self.addresses(raw)
            case "cc": cc += Self.addresses(raw)
            case "bcc": bcc += Self.addresses(raw)
            // The first one wins, as a mail client reading a header would.
            case "subject" where subject.isEmpty:
                subject = String(Self.oneLine(Self.clean(Self.decode(raw))).prefix(Self.subjectLimit))
            case "body" where body.isEmpty:
                body = String(Self.clean(Self.decode(raw), keepingNewlines: true).prefix(Self.bodyLimit))
            default: break
            }
        }
        // One address, one field, the most visible one; folded by case; and
        // no more than a person could check before sending.
        var seen = Set<String>()
        var budget = Self.recipientLimit
        func keep(_ list: [String]) -> [String] {
            list.filter { a in
                guard budget > 0, seen.insert(a.lowercased()).inserted else { return false }
                budget -= 1
                return true
            }
        }
        self.to = keep(to)
        self.cc = keep(cc)
        self.bcc = keep(bcc)
        // A bare `mailto:` (nothing after the colon, or only a `?`) is a
        // request for a blank message, as Mail opens one. A link that named
        // an address and had every one refused is not.
        guard !(self.to.isEmpty && self.cc.isEmpty && self.bcc.isEmpty)
            || !subject.isEmpty || !body.isEmpty
            || path.trimmingCharacters(in: .whitespaces).isEmpty && query.isEmpty
        else { return nil }
    }

    // MARK: - reading the link

    private static func split(_ s: Substring) -> (Substring, Substring) {
        guard let q = s.firstIndex(of: "?") else { return (s, "") }
        return (s[..<q], s[s.index(after: q)...])
    }

    /// Percent-decoded. A `+` stays a `+`: in a mailto: it is a character
    /// (`me+tag@x`), not a space.
    private static func decode<S: StringProtocol>(_ s: S) -> String {
        String(s).removingPercentEncoding ?? String(s)
    }

    /// A raw (still encoded) comma-separated address list. Split on the
    /// literal commas first, THEN each piece decoded once, and kept only if it
    /// looks like an address.
    private static func addresses<S: StringProtocol>(_ raw: S) -> [String] {
        raw.split(separator: ",").compactMap { piece in
            let a = decode(piece).trimmingCharacters(in: .whitespacesAndNewlines)
            return looksLikeAddress(a) ? a : nil
        }
    }

    static func looksLikeAddress(_ a: String) -> Bool {
        guard a.count <= 254, !a.hasPrefix("#"),
            !a.contains(where: { $0.isWhitespace || "<>\"(),;:#".contains($0) }),
            !a.unicodeScalars.contains(where: isHidden)
        else { return false }
        let parts = a.split(separator: "@", omittingEmptySubsequences: false)
        guard parts.count == 2, !parts[0].isEmpty else { return false }
        let domain = parts[1]
        return domain.contains(".") && !domain.hasPrefix(".") && !domain.hasSuffix(".")
            && !domain.contains("..")
    }

    /// Controls (Cc) and format characters (Cf: bidi overrides and
    /// isolates, zero-widths), which draw as nothing or rearrange what is
    /// drawn around them.
    private static func isHidden(_ u: Unicode.Scalar) -> Bool {
        switch u.properties.generalCategory {
        case .control, .format: return true
        default: return false
        }
    }

    /// `s` without hidden characters; a body keeps its line breaks and tabs.
    private static func clean(_ s: String, keepingNewlines: Bool = false) -> String {
        let normalized = s.replacingOccurrences(of: "\r\n", with: "\n")
            .replacingOccurrences(of: "\r", with: "\n")
        var out = String.UnicodeScalarView()
        for u in normalized.unicodeScalars {
            if keepingNewlines, u == "\n" || u == "\t" {
                out.append(u)
            } else if isHidden(u) {
                // A line break in a subject becomes a space (oneLine), not
                // nothing, so two lines do not run together.
                if u == "\n" { out.append(" ") }
            } else {
                out.append(u)
            }
        }
        return String(out)
    }

    private static func oneLine(_ s: String) -> String {
        s.components(separatedBy: .newlines).joined(separator: " ")
            .trimmingCharacters(in: .whitespaces)
    }
}

// MARK: - links from another app

/// mailto: links another app sent while Passband is the default mail app,
/// waiting for a mailbox to write from. A cold launch from a link lands while
/// the keychain is still being read; the links are held, a few of them, in
/// the order they came, until the connection is VERIFIED (the same signal the
/// notification taps wait on), and opened one at a time: the next when the
/// composer the last one opened has closed.
///
/// NOTHING GOES QUIETLY. Every link that will not open says so: the Connect
/// screen (no mailbox), a hold that ran past `holdFor`, a fourth link while
/// three wait. A link Passband cannot read at all is the store's to report.
///
/// Pure, with the clock passed in, so a headless suite can pin it.
struct MailtoInbox: Sendable {
    static let capacity = 3
    static let holdFor: TimeInterval = 60

    enum Drop: Equatable, Sendable {
        /// The Connect screen: no mailbox to write from.
        case noMailbox
        /// The connection did not come up within `holdFor`.
        case expired
        /// `capacity` links already waiting.
        case full
    }

    private(set) var held: [(href: String, at: Date, told: Bool)] = []

    var isEmpty: Bool { held.isEmpty }

    /// Hold a link. Nil when held; the reason when it was turned away.
    mutating func add(_ href: String, at now: Date) -> Drop? {
        guard held.count < Self.capacity else { return .full }
        held.append((href, now, false))
        return nil
    }

    /// The composer the last held link opened, if it is still up.
    enum Occupant: Equatable, Sendable {
        /// None up: the next link's turn.
        case none
        /// Up, and nothing written in it: the next link replaces it, as a
        /// second click on a link in a message would.
        case untouched
        /// Up, with the person's writing in it: kept, and the next link
        /// waits, said out loud (`waiting`).
        case writing
    }

    /// What to do now. `connected` is a VERIFIED connection; `occupant` is
    /// the composer the last held link opened; `gate` is the Connect screen.
    ///   * the Connect screen: every link goes, `.noMailbox`;
    ///   * connected, no occupant or an untouched one: the oldest link opens,
    ///     the rest wait their turn;
    ///   * connected, writing in the occupant: the links wait (no clock: they
    ///     wait on the person), and `waiting` is true the first time each
    ///     one is held this way, for the toast that says so;
    ///   * not connected: links held past `holdFor` go, `.expired`.
    mutating func settle(
        connected: Bool, occupant: Occupant, gate: Bool, now: Date
    ) -> (open: String?, dropped: [Drop], waiting: Bool) {
        if gate {
            let dropped = held.map { _ in Drop.noMailbox }
            held.removeAll()
            return (nil, dropped, false)
        }
        if connected {
            guard !held.isEmpty else { return (nil, [], false) }
            if occupant == .writing {
                let untold = held.contains { !$0.told }
                for i in held.indices { held[i].told = true }
                return (nil, [], untold)
            }
            return (held.removeFirst().href, [], false)
        }
        let expired = held.filter { now.timeIntervalSince($0.at) >= Self.holdFor }.count
        held.removeAll { now.timeIntervalSince($0.at) >= Self.holdFor }
        return (nil, Array(repeating: .expired, count: expired), false)
    }

    /// When the oldest held link expires, for the timer that drops it.
    var nextExpiry: Date? { held.first.map { $0.at.addingTimeInterval(Self.holdFor) } }

    /// The toast for a drop.
    static func copy(_ drop: Drop) -> String {
        switch drop {
        case .noMailbox: "Couldn't open that email link. Connect a mailbox first."
        case .expired: "Couldn't open that email link. Passband wasn't connected in time."
        case .full: "Couldn't open that email link. Too many are waiting."
        }
    }

    /// The toast for a link held behind the person's own writing.
    static let waitingCopy = "That email link will open when you send or close this message."
}
