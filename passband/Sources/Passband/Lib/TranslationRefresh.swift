// THE READER'S TRANSLATION POLL, as the two pieces of it that are arithmetic
// rather than views: when to look again, and which answer to believe. Pure and
// free of the wire types, so the suite can pin both without a reader.
//
// See docs/TRANSLATION.md, "Reader", and ThreadViewer's translation poll.

import Foundation

/// WHEN TO LOOK AGAIN for a translation that is on its way. Fast at first,
/// because most land in seconds, then a slow tail while the reader is still
/// mounted, because a long archive thread is one model call per message and
/// can take minutes. Then it stops, and the strip stops saying "translating":
/// a reader that claims forever to be translating is a reader that is wrong.
enum TranslationPollPlan {
    /// The first four waits, in seconds.
    static let fast: [Int] = [3, 6, 12, 24]
    /// The wait after those, in seconds, for as long as the budget lasts.
    static let tail = 30
    /// The most the waits may add up to, in seconds: about ten minutes.
    static let budget = 600

    /// The wait before refetch number `attempt` (counted from 0), or nil when
    /// the poll has spent its budget and should give up.
    static func delay(attempt: Int) -> Int? {
        guard attempt >= 0 else { return nil }
        if attempt < fast.count { return fast[attempt] }
        let spent = fast.reduce(0, +) + (attempt - fast.count) * tail
        return spent + tail <= budget ? tail : nil
    }
}

/// WHICH ANSWER TO BELIEVE when refetches of one thread overlap. Every refetch
/// takes a ticket before it asks and hands it back with the answer; an answer
/// is adopted only if no LATER ticket's answer has been adopted already. So a
/// GET that set off before the reader's `POST .../translate` and came back
/// after the one that followed it (which says `pending`) cannot put the stale
/// `deferred` back on screen, or into the cache.
@MainActor
final class RefreshSequence {
    private(set) var issued = 0
    private(set) var adopted = 0

    /// Take a ticket before the request goes out.
    func issue() -> Int {
        issued += 1
        return issued
    }

    /// The answer for `ticket` has arrived: true if it is the newest yet, and
    /// then it is recorded as adopted. False means a newer one already landed.
    func accept(_ ticket: Int) -> Bool {
        guard ticket > adopted else { return false }
        adopted = ticket
        return true
    }
}
