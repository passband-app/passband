// Shared badge/headline count. Membership comes from the server's FYE list;
// the client never filters it again using deadlines or importance.
import Foundation

enum NeedToday {
    static let bandLimit = 200

    static func count(_ items: [AttentionUpdate], now: Date = Date()) -> Int {
        items.count
    }
}

/// THE HERO'S ONE SERIF LINE, Mac and phone alike. A sentence about what the
/// band is ABOUT when the daemon's model wrote one (`/client/v2/fye/headline`),
/// and otherwise a line that names no count: a number set at 38pt reads as a
/// backlog, which is the feeling this screen exists to take away.
enum HeroLine {
    static func title(headline: String?, count: Int, empty: String?) -> String {
        guard count > 0 else {
            // An empty board is only "all clear" when triage has actually run.
            return empty ?? "You're all clear."
        }
        if let headline, !headline.isEmpty { return headline }
        return count == 1 ? "One thing for your eyes." : "A few things for your eyes."
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
