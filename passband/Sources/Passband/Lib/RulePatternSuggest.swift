// What the rule editor's match-pattern field should be offering while it is
// being typed, and what the offers are made of.
//
// The field takes a glob, and almost nobody arrives knowing how the sender
// spells their address. So typing in it asks the daemon's sender directory,
// two ways:
//
//   stripe      people: senders whose name or address matches, each offered as
//               their exact address, then the hosts that match, each offered
//               as `*@host`
//   *@str       hosts only: the owner has said "everyone at", so no single
//               mailbox is an answer. A bare `@str` means the same thing.
//
// A pattern with a `*` anywhere else is somebody writing a glob by hand, and
// the menu stays out of the way.
//
// Pure Foundation over the wire types, so test.sh can hold it up alone.

import Foundation

enum RulePatternSuggest {
    /// What to ask the directory for a given field text.
    enum Ask: Equatable {
        /// Nothing: the field is empty, or holds a hand-written glob.
        case nothing
        /// Hosts matching the fragment, which is empty right after `*@`.
        case domains(String)
        /// Senders matching the text, plus hosts matching `domains` when the
        /// text could be a piece of one (it has no `@` of its own).
        case senders(String, domains: String?)
    }

    static func ask(for typed: String) -> Ask {
        let text = typed.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return .nothing }
        for lead in ["*@", "@"] where text.hasPrefix(lead) {
            let rest = String(text.dropFirst(lead.count))
            // A host has no glob, no second `@` and no space in it.
            guard !rest.contains(where: { $0 == "*" || $0 == "@" || $0.isWhitespace }) else {
                return .nothing
            }
            return .domains(rest)
        }
        guard !text.contains("*") else { return .nothing }
        return .senders(text, domains: text.contains("@") ? nil : text)
    }

    /// One row of the menu. `pattern` is what accepting it writes in the field.
    struct Suggestion: Identifiable, Equatable {
        enum Kind: Equatable {
            /// One mailbox, with the display name it writes under.
            case sender(name: String?)
            /// Everyone at a host. `senders` is how many addresses that is,
            /// nil when the daemon could not say (see `domains(from:)`).
            case domain(host: String, senders: Int?)
        }
        var pattern: String
        var kind: Kind
        /// Mail received from whoever the pattern covers.
        var messages: Int

        var id: String { pattern }
    }

    /// The menu for one answer: people first, then hosts. A row that would
    /// write what the field already says is not an offer and is dropped, so a
    /// finished pattern does not sit under a menu of itself.
    static func suggestions(
        typed: String, senders: [SenderHit], domains: [SenderDomainHit]
    ) -> [Suggestion] {
        let current = typed.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        var seen: Set<String> = [current]
        var rows: [Suggestion] = []
        for hit in senders {
            guard seen.insert(hit.addr.lowercased()).inserted else { continue }
            let name = hit.display_name.flatMap { $0.isEmpty ? nil : $0 }
            rows.append(
                Suggestion(pattern: hit.addr, kind: .sender(name: name), messages: hit.msg_count))
        }
        for hit in domains {
            let pattern = "*@\(hit.domain)"
            guard seen.insert(pattern.lowercased()).inserted else { continue }
            rows.append(
                Suggestion(
                    pattern: pattern, kind: .domain(host: hit.domain, senders: hit.sender_count),
                    messages: hit.msg_count))
        }
        return rows
    }

    /// The directory query that stands in for `/client/sender-domains` on a
    /// daemon older than the route: senders whose address has `@fragment` in
    /// it, which is every sender at a host starting with the fragment.
    static func fallbackQuery(_ fragment: String) -> String { "@\(fragment)" }

    /// Hosts rolled up from sender hits, for that same older daemon. The hits
    /// are a ranked PAGE of the directory, not all of it, so the totals are a
    /// floor and the sender count is not claimed at all.
    static func domains(
        from hits: [SenderHit], matching fragment: String, limit: Int
    ) -> [SenderDomainHit] {
        let want = fragment.lowercased()
        var order: [String] = []
        var totals: [String: Int] = [:]
        for hit in hits {
            guard let at = hit.addr.lastIndex(of: "@") else { continue }
            let host = hit.addr[hit.addr.index(after: at)...].lowercased()
            guard !host.isEmpty, host.hasPrefix(want) else { continue }
            if totals[host] == nil { order.append(host) }
            totals[host, default: 0] += hit.msg_count
        }
        // Stable on first appearance, so a tie keeps the directory's ranking.
        let ranked = order.enumerated().sorted {
            let (a, b) = (totals[$0.element] ?? 0, totals[$1.element] ?? 0)
            return a != b ? a > b : $0.offset < $1.offset
        }
        return ranked.prefix(limit).map {
            SenderDomainHit(domain: $0.element, sender_count: nil, msg_count: totals[$0.element] ?? 0)
        }
    }
}
