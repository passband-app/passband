// The quick clear sprint's rules, with nothing that draws or holds state, so
// the test suite pins them: which item is next, which are already handled,
// how far along the sprint is, how the clock reads, and what the end says.
// The sprint itself is Model/QuickClearSprint.swift; the frame is
// Model/QuickClearBanner.swift.

import Foundation

enum QuickClearQueue {
    typealias Item = QuickClearSignal.Item

    /// The items still to go, in queue order: not handled (here or anywhere
    /// else) and not passed over in this sprint. The first is on screen.
    ///
    /// Derived, never stored: an undo takes the id back out of `handled`,
    /// and the item comes back to the front exactly where it was.
    static func pending(_ items: [Item], handled: Set<Int>, skipped: Set<Int>) -> [Item] {
        items.filter { !handled.contains($0.message_id) && !skipped.contains($0.message_id) }
    }

    /// Whether a cached row says the mail was dealt with since the frame:
    /// done, archived, or parked behind a reminder. Another device's done
    /// reaches the board's rows on the next poll, which is how it reaches
    /// here. nil (no cached row) is "not known to be handled", never handled.
    static func handledElsewhere(_ row: AttentionUpdate?) -> Bool {
        guard let row else { return false }
        return row.status == .done || !(row.resolved_at ?? "").isEmpty || row.hasPendingReminder
    }

    /// "4 of 14": the 1-based place of the item on screen, counting every
    /// item behind it, handled or passed. Never past the total.
    static func position(total: Int, pending: Int) -> Int {
        guard total > 0 else { return 0 }
        return min(total, max(1, total - pending + 1))
    }

    /// "1:05", "12:00": a clock face for the header.
    static func clock(_ secs: Int) -> String {
        let s = max(0, secs)
        return "\(s / 60):" + String(format: "%02d", s % 60)
    }

    /// The clock's line once the budget is spent: said once, no number
    /// climbing at the owner. "5 minutes are up, no rush".
    static func timeUpLine(_ budget: Int) -> String {
        let m = max(1, Int((Double(budget) / 60).rounded()))
        return m == 1 ? "1 minute is up, no rush" : "\(m) minutes are up, no rush"
    }

    /// The item's tag: the daemon's kind word, as words ("receipt",
    /// "past event"). nil when there is none worth showing.
    static func kindLabel(_ kind: String?) -> String? {
        guard let kind else { return nil }
        let words = kind.replacingOccurrences(of: "_", with: " ").trimmingCharacters(in: .whitespaces)
        guard !words.isEmpty, words.count <= 24, words.lowercased() != "unknown" else { return nil }
        // The sweep's own rows only reach the screen when the sweep was
        // passed over; "smart clear" on them would read as a verb.
        if kind == QuickClearSignal.Item.smartClearKind { return "expired" }
        return words.lowercased()
    }

    /// The item's time, as the daemon guessed it: "8 sec", "2 min".
    static func estimateLabel(_ secs: Int?) -> String? {
        guard let secs, secs > 0 else { return nil }
        if secs < 60 { return "\(secs) sec" }
        return "\(max(1, Int((Double(secs) / 60).rounded()))) min"
    }

    /// How a sprint ended, for analytics: the queue ran out, the time did, or
    /// the owner closed it first.
    enum Outcome: String, Sendable {
        case emptied
        case time_up
        case closed
    }

    static func outcome(pending: Int, elapsed: Int, budget: Int) -> Outcome {
        if pending == 0 { return .emptied }
        return elapsed >= budget ? .time_up : .closed
    }

    /// The end card's headline: "Cleared 12 in 4 minutes". Warm and short.
    static func summary(cleared: Int, elapsed: Int, left: Int) -> String {
        guard cleared > 0 else { return left == 0 ? "All clear already" : "Nothing cleared this time" }
        let when: String
        if elapsed < 60 {
            when = "in under a minute"
        } else {
            let m = max(1, Int((Double(elapsed) / 60).rounded()))
            when = m == 1 ? "in 1 minute" : "in \(m) minutes"
        }
        return "Cleared \(cleared) \(when)"
    }

    /// The line under it. Says what is left, never what was missed.
    static func summaryNote(cleared: Int, left: Int, timeUp: Bool) -> String {
        if left == 0 {
            return cleared > 0 ? "That was the whole queue. Nice work." : "Everything in this queue was handled already."
        }
        let rest = left == 1 ? "1 is still in the queue" : "\(left) are still in the queue"
        if timeUp { return "Time is up. \(rest) if you want to keep going." }
        return cleared > 0 ? "\(rest) for later." : "Everything is right where you left it."
    }
}
