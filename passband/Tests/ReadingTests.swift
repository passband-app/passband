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
        precondition(pruned[0].latestThreadId == "thread-1" && pruned[0].count == 1)
        precondition(Reading.derive(updates: [], rules: []).isEmpty, "Empty canonical membership clears cards")

        // The card is titled from the feed's `from_name`. The feed did not
        // carry one, so a mailer whose local-part is a hash was titled with
        // the hash. The second item is an OLDER DAEMON's: no key at all, and
        // it must still decode and fall back to the address.
        let feed = try JSONDecoder().decode(AgentFeed.self, from: Data("""
            {"version":2,"ranked_at":"2026-09-30T12:00:00Z","items":[
              {"message_id":7,"thread_id":"mopar","from_addr":"21ebc25a592052689288153b3e3fb858@emails.revolutionparts.com",
               "from_name":"Mopar.com National eStore",
               "subject":"Fall Into Shipping Savings","received_at":"2026-09-30T11:00:00Z","score":1,
               "decision":{"kinds":["promotional"],"destinations":["reading"],"summary":"New gear","reason":"Bulk"},
               "attention":{"show_in_fye":false,"state":"informational","summary":"",
                 "factors":{"urgency":0,"action_need":0,"personal_relevance":0,"importance":0,"attention_at":null}}},
              {"message_id":8,"thread_id":"old","from_addr":"offers@cmail.dickblick.com",
               "subject":"Sale","received_at":"2026-09-29T11:00:00Z","score":1,
               "decision":{"kinds":["promotional"],"destinations":["reading"],"summary":"Sale","reason":"Bulk"},
               "attention":{"show_in_fye":false,"state":"informational","summary":"",
                 "factors":{"urgency":0,"action_need":0,"personal_relevance":0,"importance":0,"attention_at":null}}}
            ]}
            """.utf8))
        let named = Reading.derive(updates: feed.items.map(\.readingRow), rules: [])
        precondition(named.count == 2)
        precondition(
            SenderID.displayName(named[0].sender) == "Mopar.com National eStore",
            "A feed card is titled with the sender's display name, not its address")
        precondition(
            named[0].address == "21ebc25a592052689288153b3e3fb858@emails.revolutionparts.com",
            "The name never becomes the grouping key")
        precondition(
            named[1].sender == "offers@cmail.dickblick.com",
            "A feed with no from_name still decodes and falls back to the address")
        print("ok: 9 canonical Reading checks passed")
    }
}
