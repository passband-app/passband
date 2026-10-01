// Reading cards group the messages selected by the triage agent.
// Sender preferences are shown as context, never used to filter this feed.

import Foundation

/// A Reading card groups agent-selected messages from one sender.
struct ReadingSender: Identifiable, Hashable, Sendable {
    /// Grouping key = bare lowercased address.
    var address: String
    /// A representative raw sender string (for avatar + display name).
    var sender: String
    /// Count of selected messages in the window.
    var count: Int
    /// Latest one_line in the window (the summary line).
    var summary: String
    /// Latest message date. Cards sort content first, then newest-first.
    var latest: Double
    /// The latest message's thread — clicking the card opens this email.
    var latestThreadId: String
    /// The window's updates, NEWEST FIRST — the viewer's horizontal queue (h/l
    /// between this sender's emails) and the bulk-done target.
    var items: [AttentionUpdate]
    /// The rule governing this sender, if any (drives the chip).
    var rule: SenderRule?
    /// The items triage judged something to read (article, essay, post)
    /// rather than a promotion.
    var contentIds: Set<Int> = []
    /// Content older than this (epoch seconds) has had its turn at the top and
    /// files back in by date like everything else. Stamped by `derive` so the
    /// answer is fixed for one derivation, and `prune` keeps it.
    var contentFreshSince: Double = 0

    /// Content cards sort ahead of the rest and get their own tint, for
    /// `Reading.contentWindow` after they arrive. Taken from the newest message,
    /// like `summary`, so the card's placement and the summary it shows always
    /// agree, and `prune` stays right for free.
    var isContent: Bool {
        guard let newest = items.first, contentIds.contains(newest.id) else { return false }
        return latest >= contentFreshSince
    }

    var id: String { address }
}

enum Reading {
    /// How long content keeps its place at the top of the board.
    static let contentWindow: TimeInterval = 7 * 24 * 3600

    /// The adapter carries the server's real received timestamp here.
    private static func dateOf(_ update: AttentionUpdate) -> Double {
        Fmt.date(update.surfaced_at)?.timeIntervalSince1970 ?? 0
    }

    /// Glob match for a rule's match_pattern ("*@acme.com") against a bare
    /// address. `*` matches any run; case-insensitive.
    static func ruleMatches(pattern: String, address: String) -> Bool {
        let pat = pattern.trimmingCharacters(in: .whitespaces).lowercased()
        guard !pat.isEmpty else { return false }
        var rx = "^"
        for ch in pat {
            if ch == "*" {
                rx += ".*"
            } else if ".+?^${}()|[]\\".contains(ch) {
                rx += "\\\(ch)"
            } else {
                rx.append(ch)
            }
        }
        rx += "$"
        if let re = try? Regex(rx) {
            return address.lowercased().wholeMatch(of: re) != nil
        }
        // Pragmatic fallback: does the pattern's domain appear in the address?
        let dom = pat.split(separator: "@").last.map(String.init) ?? pat
        return address.lowercased().contains(dom.replacingOccurrences(of: "*", with: ""))
    }

    /// Find the rule governing an address. Prefers the MOST SPECIFIC (fewest
    /// wildcards) match so the chip is stable.
    static func rule(for address: String, in rules: [SenderRule]) -> SenderRule? {
        let hits = rules.filter { ruleMatches(pattern: $0.match_pattern, address: address) }
        return hits.min {
            $0.match_pattern.filter { c in c == "*" }.count
                < $1.match_pattern.filter { c in c == "*" }.count
        }
    }

    /// Group messages already selected for Reading by the agent. Grouping is
    /// presentation only: no sender shape, category, score or repetition test.
    ///
    /// `content` holds the ids triage judged to be content rather than
    /// promotion; those senders sort first, newest-first within each half, and
    /// the cap applies after, so a busy week of promotions never pushes the
    /// reading off the board. Only for `contentWindow`: an unread essay from
    /// last month should not sit above today's mail forever.
    static func derive(
        updates: [AttentionUpdate], rules: [SenderRule], content: Set<Int> = [],
        now: Date = Date(), limit: Int = 24
    ) -> [ReadingSender] {
        let freshSince = now.timeIntervalSince1970 - contentWindow
        let groups = Dictionary(grouping: updates) { SenderID.address($0.sender) }
        return groups.compactMap { address, messages -> ReadingSender? in
            let ordered = messages.sorted {
                let lhs = dateOf($0), rhs = dateOf($1)
                return lhs == rhs ? $0.id > $1.id : lhs > rhs
            }
            guard let latest = ordered.first else { return nil }
            return ReadingSender(
                address: address, sender: latest.senderString, count: ordered.count,
                summary: latest.one_line, latest: dateOf(latest),
                latestThreadId: latest.thread_id, items: ordered,
                rule: rule(for: address, in: rules),
                contentIds: content.intersection(ordered.map(\.id)),
                contentFreshSince: freshSince)
        }
        .sorted(by: boardOrder)
        .prefix(limit).map { $0 }
    }

    /// Content before promotions, newest first within each half.
    private static func boardOrder(_ lhs: ReadingSender, _ rhs: ReadingSender) -> Bool {
        if lhs.isContent != rhs.isContent { return lhs.isContent }
        return lhs.latest == rhs.latest ? lhs.address < rhs.address : lhs.latest > rhs.latest
    }

    /// The `*@domain` pattern a newsletter CTA prefills into the rule editor.
    /// Drop already-resolved messages from a derived window, recomputing the
    /// fields taken from the newest survivor and removing any sender left with
    /// nothing at all.
    ///
    /// A READ-SIDE FILTER, not a mutation of the store. `derive` skips
    /// `status == .done`, so the server's next poll produces this same answer —
    /// this only covers the ten seconds in between, which is exactly the window
    /// in which marking mail done looked like it had not worked. `resolvedIds`
    /// is already the app's record of "resolved, poll has not caught up", and
    /// undo clears it, so a restored message brings its card straight back.
    ///
    /// Re-sorted after: a sender placed by a promotion that is now done may
    /// have an essay underneath, and that card belongs with the content.
    static func prune(_ senders: [ReadingSender], resolved: Set<Int>) -> [ReadingSender] {
        guard !resolved.isEmpty else { return senders }
        return senders.compactMap { nl -> ReadingSender? in
            let live = nl.items.filter { !resolved.contains($0.id) }
            if live.count == nl.items.count { return nl }
            // Nothing left in the window: the card goes, rather than sitting
            // there at zero until the poll agrees.
            guard let newest = live.first else { return nil }
            var out = nl
            out.items = live
            out.count = live.count
            // `derive` sorts items newest-first and takes these three from the
            // newest message, so the head of the surviving list carries them.
            out.latest = dateOf(newest)
            out.latestThreadId = newest.thread_id
            if !newest.one_line.isEmpty { out.summary = newest.one_line }
            return out
        }
        .sorted(by: boardOrder)
    }

    static func domainPattern(_ address: String) -> String {
        let domain =
            SenderID.faviconDomain(address) ?? address.split(separator: "@").last.map(String.init)
            ?? address
        return "*@\(domain)"
    }

    /// Strip redundant genre labels ("Promotional email from X:", "Newsletter:
    /// …") — the section already says what these are. Only recognized leading
    /// shapes are removed; anything else passes through unchanged.
    static func cleanSummary(_ summary: String) -> String {
        var out = summary.trimmingCharacters(in: .whitespaces)
        out = out.replacing(
            /(?i)^(promotional email|marketing email|newsletter|promo(?:tion)?)\s*(from\s+[^:,-]+)?[:,-]?\s+/,
            with: "")
        out = out.replacing(/(?i)^\w[\w ]{0,24}?\bpromotion\s+(for|from|of)\s+/, with: "")
        out = out.trimmingCharacters(in: .whitespaces)
        if out.isEmpty { return summary.trimmingCharacters(in: .whitespaces) }
        return Fmt.capitalizingFirst(out)
    }
}
