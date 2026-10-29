// The `from:` operator as the search field sees it while it is being typed.
//
// The daemon parses `from:<text>` out of a finished query (search_query.rs);
// this is the OTHER end of that operator, the one that decides when the search
// field should be offering senders and what accepting one does to the text.
//
// THE TRAILING TOKEN IS THE CARET. A SwiftUI TextField does not tell us where
// the caret is, and RecipientField gets by without it by parsing the whole
// string into pills and a fragment. The search field's equivalent: the reader
// is "in" a `from:` operator exactly while the LAST whitespace-separated token
// starts with `from:` and the text does not end in whitespace. A space after
// the address closes the operator (the way it closes a recipient pill), and a
// `from:` earlier in the query is finished business the reader has moved past.
// Wrong in a way anybody can see: if the menu is up and you did not want it,
// type a space.
//
// Pure Foundation, no SwiftUI, so test.sh can hold it up alone.

import Foundation

enum FromOperator {
    /// The operator's spelling, matched case-insensitively so `From:` and
    /// `FROM:` open the menu too. The daemon's parser is the same way.
    private static let prefix = "from:"

    /// The text after `from:` in the trailing token, "" when the reader has
    /// typed the operator and nothing after it yet (the menu then lists the
    /// senders with the most mail), and nil when the reader is not in a
    /// `from:` operator at all.
    static func fragment(in query: String) -> String? {
        guard let token = trailingToken(of: query) else { return nil }
        guard token.count >= prefix.count,
            token.prefix(prefix.count).lowercased() == prefix
        else { return nil }
        return String(token.dropFirst(prefix.count))
    }

    /// The query with the trailing `from:` token replaced by `from:<address>`
    /// and a closing space, so the operator is complete and the next keystroke
    /// starts a new word. A query that is not in a `from:` operator comes back
    /// unchanged: accepting a suggestion that is no longer on offer must not
    /// rewrite the reader's text.
    static func accepting(_ address: String, in query: String) -> String {
        guard fragment(in: query) != nil, let token = trailingToken(of: query) else {
            return query
        }
        let head = query.dropLast(token.count)
        return "\(head)\(prefix)\(address) "
    }

    /// True when the query has no searchable content YET: every token is an
    /// operator with nothing after its colon (`from:`, `after:`, `before:`).
    /// The daemon drops a valueless operator and then refuses the empty search
    /// with a 400, which is right for a finished query and wrong for the half
    /// second in which the reader has typed `from:` and the menu is opening;
    /// the panel uses this to wait rather than to show the refusal.
    static func awaitingValue(in query: String) -> Bool {
        let tokens = query.split(whereSeparator: { $0.isWhitespace || $0.isNewline })
        guard !tokens.isEmpty else { return false }
        return tokens.allSatisfy { token in
            operators.contains(token.lowercased())
        }
    }

    // MARK: - a sender typed as plain words

    /// The operators the daemon parses that take a value, lowercased.
    private static let operators = ["from:", "to:", "after:", "before:"]

    /// The daemon's one operator that carries its own value: complete as
    /// typed, so never "awaiting" one. `from:me` means the same.
    private static let scopeOperator = "in:sent"


    /// Past this many words the reader is describing mail, not naming a person.
    private static let senderWords = 3

    /// What to look senders up for when the reader has typed plain words and no
    /// operator: the whole query, trimmed. nil when there is nothing to offer
    /// for: one character (every sender matches it), a query that already
    /// carries an operator (the reader has said who, or is saying it through
    /// the `from:` menu), or more words than a name has.
    static func senderFragment(in query: String) -> String? {
        let tokens = query.split(whereSeparator: { $0.isWhitespace || $0.isNewline })
        guard !tokens.isEmpty, tokens.count <= senderWords else { return nil }
        guard !tokens.contains(where: { isOperator($0) }) else { return nil }
        let fragment = tokens.joined(separator: " ")
        return fragment.count >= 2 ? fragment : nil
    }

    /// Whether a sender is still an answer to `fragment`, by the directory's own
    /// test (a substring of the address or the display name, case folded). The
    /// panel holds the last fetch while the next is in flight; this is what
    /// keeps a row the new keystroke ruled out from being offered meanwhile.
    static func matches(addr: String, name: String?, fragment: String) -> Bool {
        let needle = fragment.lowercased()
        return addr.lowercased().contains(needle)
            || (name?.lowercased().contains(needle) ?? false)
    }

    /// The query that lists one sender's mail: the operator, the address, and
    /// the closing space `accepting` leaves, so the next keystroke narrows the
    /// listing rather than editing the address.
    static func listing(of address: String) -> String {
        "\(prefix)\(address) "
    }

    /// True when the query is operators and nothing else, each with a value
    /// (`from:dan@example.com`, `from:dan after:2026-01-01`). There are no
    /// words to rank on, so the daemon answers with a plain newest-first page,
    /// and the shells draw it as one list in date order rather than split into
    /// Not done and Done.
    static func isListing(_ query: String) -> Bool {
        let tokens = query.split(whereSeparator: { $0.isWhitespace || $0.isNewline })
        guard !tokens.isEmpty else { return false }
        return tokens.allSatisfy { token in
            let lower = token.lowercased()
            return lower == scopeOperator
                || operators.contains { lower.hasPrefix($0) && lower.count > $0.count }
        }
    }

    private static func isOperator(_ token: Substring) -> Bool {
        let lower = token.lowercased()
        return lower == scopeOperator || operators.contains { lower.hasPrefix($0) }
    }

    /// The last whitespace-separated token, or nil when the text is empty or
    /// ends in whitespace (the caret is past the last token, so no token is
    /// being typed).
    private static func trailingToken(of query: String) -> Substring? {
        guard let last = query.unicodeScalars.last,
            !CharacterSet.whitespacesAndNewlines.contains(last)
        else { return nil }
        if let cut = query.lastIndex(where: { $0.isWhitespace || $0.isNewline }) {
            return query[query.index(after: cut)...]
        }
        return query[...]
    }
}

/// HOW A PANEL SEARCH ASKS FOR ITS ORDER. Beside `FromOperator.isListing`
/// because it is the same question asked of the same query, and because this
/// file compiles alone in test.sh while `SearchFetch` cannot.
///
/// PLAIN RANKED ORDER on a daemon that advertises `search_plain_order`: no
/// status grouping, sent mail kept by `include_sent`. On an older daemon the
/// old ask, `unfinished_first` for anything but a listing, because that flag
/// is also what brings sent mail into its keyword searches.
///
/// A LISTING is asked exactly as before on either daemon: newest first, no
/// grouping and no `include_sent`, so a picked sender's page does not change.
///
/// Semantic/hybrid pages in either order come back as a `recall:` cursor,
/// which that daemon accepts beside either ask, so a scroll that straddles
/// the flag landing is not refused (its next page is simply in the new order).
enum SearchOrder {
    static func ask(_ term: String, plainOrder: Bool) -> (unfinishedFirst: Bool, includeSent: Bool) {
        if FromOperator.isListing(term) { return (false, false) }
        return plainOrder ? (false, true) : (true, false)
    }
}
