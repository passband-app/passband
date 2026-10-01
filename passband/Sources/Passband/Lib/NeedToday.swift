// The phone badge's count (Mac dock and sitrep headline show none). Membership
// comes from the server's FYE list; the client never filters it again using
// deadlines or importance.
import Foundation

enum NeedToday {
    static let bandLimit = 200

    static func count(_ items: [AttentionUpdate], now: Date = Date()) -> Int {
        items.count
    }
}

/// THE HERO'S ONE SERIF LINE, Mac and phone alike. Good news about the mail
/// when the daemon's model found some (`/client/v2/fye/headline`), and
/// otherwise a line that is calm and true on any board. Never a count and
/// never a chore: this line is a boost of confidence, and the rows beneath it
/// are where the work lives.
enum HeroLine {
    static func title(headline: String?, count: Int, empty: String?) -> String {
        guard count > 0 else {
            // An empty board is only "all clear" when triage has actually run.
            return empty ?? "You're all clear."
        }
        if let headline, !headline.isEmpty { return headline }
        return "Your inbox, quietly sorted."
    }
}

/// Process snapshots server order, but consumes current row content and state.
enum ProcessQueue {
    static func pending(snapshot: [AttentionUpdate], live: [AttentionUpdate], handled: Set<Int>) -> [AttentionUpdate] {
        let current = Dictionary(live.map { ($0.id, $0) }, uniquingKeysWith: { _, newest in newest })
        return snapshot.compactMap { row in
            guard !handled.contains(row.id), let latest = current[row.id], latest.status != .done else { return nil }
            return latest
        }
    }
}
