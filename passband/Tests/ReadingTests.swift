import Foundation

@main
struct ReadingTests {
    static func main() throws {
        func row(_ id: Int, sender: String, reason: String, date: String) -> AttentionUpdate {
            AttentionUpdate(id: id, thread_id: "thread-\(id)", tier: .signal,
                importance: 100, sender: sender, one_line: "Message \(id)", reason: reason,
                status: .new, surfaced_at: date)
        }
        let oldHuman = row(1, sender: "friend@example.com", reason: "Personal note",
            date: "2020-01-01T00:00:00Z")
        let receiptCopy = row(2, sender: "shop@example.com", reason: "Order confirmation / receipt",
            date: "2026-09-01T00:00:00Z")
        let recentHuman = row(3, sender: "friend@example.com", reason: "Sale promotion",
            date: "2026-09-02T00:00:00Z")
        let cards = Reading.derive(updates: [oldHuman, receiptCopy, recentHuman], rules: [])
        precondition(cards.count == 2, "No sender, copy, age, or tier may remove agent-selected mail")
        precondition(cards[0].items.map(\.id) == [3, 1], "Grouping retains every message, newest first")
        precondition(cards[1].items.map(\.id) == [2], "Receipt wording is not a client exclusion")
        let pruned = Reading.prune(cards, resolved: [3])
        let friend = pruned.first { $0.address == "friend@example.com" }!
        precondition(friend.latestThreadId == "thread-1" && friend.count == 1)
        precondition(pruned.map(\.address) == ["shop@example.com", "friend@example.com"],
            "Prune re-sorts the way the next poll will")
        precondition(Reading.derive(updates: [], rules: []).isEmpty, "Empty canonical membership clears cards")
        // No silent cap: the zone folds instead, and its count is the real one.
        let many = (0..<40).map { row(100 + $0, sender: "s\($0)@news.com", reason: "Sale",
            date: "2026-09-01T00:00:00Z") }
        precondition(Reading.derive(updates: many, rules: []).count == 40, "derive keeps every sender")

        // Pinned so every essay below is inside the content window.
        let pinned = Fmt.date("2026-09-11T00:00:00Z")!
        // Content (articles, essays, posts) sorts ahead of promotions, newest
        // first within each half, and the cap applies after the split.
        let promoNew = row(10, sender: "deals@store.com", reason: "Sale", date: "2026-09-20T00:00:00Z")
        let essayOld = row(11, sender: "writer@substack.com", reason: "Essay", date: "2026-09-10T00:00:00Z")
        let postMid = row(12, sender: "blog@lab.dev", reason: "Post", date: "2026-09-15T00:00:00Z")
        let split = Reading.derive(
            updates: [promoNew, essayOld, postMid], rules: [], content: [11, 12], now: pinned)
        precondition(split.map(\.address) == ["blog@lab.dev", "writer@substack.com", "deals@store.com"],
            "Content first, newest-first within each half")
        precondition(split.map(\.isContent) == [true, true, false])
        let capped = Reading.derive(
            updates: [promoNew, essayOld], rules: [], content: [11], now: pinned, limit: 1)
        precondition(capped.map(\.address) == ["writer@substack.com"], "A promotion never pushes content off the board")

        // A sender is placed by its NEWEST message, like its summary, and prune
        // re-derives that when the newest is done.
        let writerPromo = row(13, sender: "writer@substack.com", reason: "Merch", date: "2026-09-21T00:00:00Z")
        let mixed = Reading.derive(updates: [essayOld, writerPromo], rules: [], content: [11], now: pinned)
        precondition(!mixed[0].isContent, "Newest is a promotion")
        precondition(Reading.prune(mixed, resolved: [13])[0].isContent, "Done promo uncovers the essay")
        let board = Reading.derive(updates: [promoNew, essayOld, writerPromo], rules: [], content: [11], now: pinned)
        precondition(board.map(\.address) == ["writer@substack.com", "deals@store.com"], "Both promotions: newest first")
        let postOld = row(14, sender: "blog@lab.dev", reason: "Post", date: "2026-09-05T00:00:00Z")
        let before = Reading.derive(updates: [promoNew, postOld, essayOld, writerPromo], rules: [], content: [11, 14], now: pinned)
        precondition(before.map(\.address) == ["blog@lab.dev", "writer@substack.com", "deals@store.com"])
        let uncovered = Reading.prune(before, resolved: [13])
        precondition(uncovered.map(\.address) == ["writer@substack.com", "blog@lab.dev", "deals@store.com"],
            "Prune re-sorts an uncovered essay into the content half")

        // The wire: reading_kind wins; decisions stored before it fall back to kinds.
        func item(_ kinds: String, _ readingKind: String?) throws -> AgentFeedItem {
            let rk = readingKind.map { ",\"reading_kind\":\"\($0)\"" } ?? ""
            let json = """
            {"message_id":1,"thread_id":"t","from_addr":"a@b.c","subject":"s","received_at":"2026-09-01T00:00:00Z","score":0,
             "decision":{"kinds":\(kinds),"destinations":["reading"]\(rk),"summary":"x","reason":"y"},
             "attention":{"show_in_fye":false,"state":"informational","summary":"",
               "factors":{"urgency":0,"action_need":0,"personal_relevance":0,"importance":0,"attention_at":null}}}
            """
            return try JSONDecoder().decode(AgentFeedItem.self, from: Data(json.utf8))
        }
        let promotedByKind = try item(#"["promotional"]"#, "content")
        let demotedByKind = try item(#"["editorial"]"#, "promotion")
        let legacyEditorial = try item(#"["editorial"]"#, nil)
        let legacyMixed = try item(#"["editorial","promotional"]"#, nil)
        precondition(promotedByKind.isReadingContent, "reading_kind outranks kinds")
        precondition(!demotedByKind.isReadingContent)
        precondition(legacyEditorial.isReadingContent, "Legacy editorial reads as content")
        precondition(!legacyMixed.isReadingContent, "Legacy mixed reads as promotion")
        // Content keeps the top for a week, then files back in by date.
        let now = Fmt.date("2026-09-30T00:00:00Z")!
        let staleEssay = row(20, sender: "old@essays.com", reason: "Essay", date: "2026-09-01T00:00:00Z")
        let freshPromo = row(21, sender: "deals@store.com", reason: "Sale", date: "2026-09-29T00:00:00Z")
        let weekEssay = row(22, sender: "new@essays.com", reason: "Essay", date: "2026-09-24T00:00:00Z")
        let aged = Reading.derive(
            updates: [staleEssay, freshPromo, weekEssay], rules: [], content: [20, 22], now: now)
        precondition(aged.map(\.address) == ["new@essays.com", "deals@store.com", "old@essays.com"],
            "Fresh content first, stale content sorts by date")
        precondition(aged.map(\.isContent) == [true, false, false], "Stale content loses the tint too")
        print("ok: 20 canonical Reading checks passed")
    }
}
