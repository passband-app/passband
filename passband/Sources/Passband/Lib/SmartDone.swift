// SMART DONE, IN WORDS. One press on the Reading zone and the daemon marks
// expired and plainly unwanted mail done; this is what the receipt says came
// of it. Every line is something that went away, put the way a person would
// say it, and the reasons are kept apart because "5 offers that ended" is a
// fact somebody can agree with where "43 cleared" is only a number.
//
// Foundation only, so test.sh compiles it alone. It takes the daemon's reason
// codes as plain strings for the same reason: no wire type rides along.

import Foundation

enum SmartDoneCopy {
    /// The daemon's reason codes, in the order the receipt lists them: the
    /// ones nobody could argue with first.
    static let order = ["event_passed", "offer_ended", "expired", "ignored_sender", "stale_promo"]

    /// "43 emails cleared". The receipt's headline.
    static func headline(cleared: Int) -> String {
        cleared == 1 ? "1 email cleared" : "\(cleared) emails cleared"
    }

    /// "from 17 senders". nil when the daemon did not say.
    static func senders(_ count: Int) -> String? {
        guard count > 0 else { return nil }
        return count == 1 ? "from 1 sender" : "from \(count) senders"
    }

    /// One reason, as a count and the words after it, so the receipt can set
    /// the number in its own column.
    struct Line: Hashable, Sendable {
        var count: Int
        var text: String
        /// "5 offers that ended": the line as one sentence, for VoiceOver.
        var spoken: String { "\(count) \(text)" }
    }

    /// One line per reason that cleared something, in `order`. Codes this
    /// build has no words for are summed into one last plain line, so the
    /// lines always account for everything a newer daemon cleared.
    static func lines(byReason: [String: Int]) -> [Line] {
        var out: [Line] = []
        for code in order {
            guard let n = byReason[code], n > 0, let text = phrase(code, n) else { continue }
            out.append(Line(count: n, text: text))
        }
        let other = byReason.filter { !order.contains($0.key) && $0.value > 0 }.values.reduce(0, +)
        if other > 0 { out.append(Line(count: other, text: other == 1 ? "other" : "others")) }
        return out
    }

    private static func phrase(_ code: String, _ n: Int) -> String? {
        let one = n == 1
        switch code {
        case "event_passed": return one ? "event that already happened" : "events that already happened"
        case "offer_ended": return one ? "offer that ended" : "offers that ended"
        // Judged out of date by the model, which does not say what KIND of
        // mail it was: so neither does this.
        case "expired": return "no longer current"
        case "ignored_sender": return "from senders you never open"
        case "stale_promo": return one ? "promotion you never opened" : "promotions you never opened"
        default: return nil
        }
    }

    /// The model pass was meant to run and did not. `skipped_unconfigured`
    /// and `skipped_nothing_to_ask` are not this: nothing was missed.
    static func modelMissed(_ model: String?) -> Bool {
        model == "skipped_failed" || model == "skipped_cap"
    }

    /// Said under a receipt, or with `nothing`, when the closer look did not
    /// happen: what was cleared is still true, and it is not the whole job.
    static let modelMissedNote = "The closer look did not run this time."

    /// Nothing was cleared. A toast, never a receipt.
    static func nothing(model: String?) -> String {
        modelMissed(model)
            ? "Nothing obvious to clear. \(modelMissedNote)" : "Reading is already current."
    }

    /// This daemon has no smart done route.
    static let unavailable = "Smart done needs a server update."
    static let failed = "Smart done did not finish. Try again."
    /// Another of the owner's devices pressed it first; the daemon runs one
    /// press at a time. Not an error, and nothing to retry.
    static let alreadyRunning = "Smart done is already running."

    /// After an undo. `reopened` is the daemon's count of what came back.
    static func undone(reopened: Int) -> String {
        reopened == 1 ? "1 email is back in Reading." : "\(reopened) emails are back in Reading."
    }
}
