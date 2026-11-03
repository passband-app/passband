// A mailto: link clicked inside a message, read into what a new composer
// opens with (RFC 6068).
//
// The link comes from the MAIL, so it is untrusted: it only ever fills in a
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
    /// link, is too long to be one, or asks for nothing at all. A link with a
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
        guard !(self.to.isEmpty && self.cc.isEmpty && self.bcc.isEmpty)
            || !subject.isEmpty || !body.isEmpty
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
