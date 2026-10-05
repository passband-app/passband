// How long a message was actually in front of somebody, and whether they
// clicked anything in it. The purge flow's reasons ("about 4 seconds each",
// "never clicked") are only as honest as these two numbers, and both are easy
// to get wrong in ways no build notices: time that kept running behind another
// app, an arrow-key pass counted as a read, one forgotten window outweighing a
// year of mail. So the arithmetic lives here, as a value with no clock of its
// own, and the reader only tells it what happened and when.
//
// COUNTS ONLY. Nothing in here ever holds a URL, a host, a subject or any
// other string from the mail: a message id, milliseconds and a tally.

import Foundation

struct AttentionClock {
    /// What one message is owed since the last drain.
    struct Report: Equatable, Sendable {
        let messageId: Int
        let dwellMs: UInt64
        let linkClicks: UInt32
    }

    /// A stint shorter than this contributes no dwell. Walking a thread with
    /// the arrow keys puts every message at the focus line for a few hundred
    /// milliseconds, and "you spend 300 ms on these" is a statement about the
    /// keyboard, not about the sender. Its clicks still count: a click is a
    /// deliberate act however briefly the message was up.
    static let minimumStintMs: UInt64 = 1_000

    /// The most one stint can contribute: somebody walked away with the window
    /// focused. The daemon caps too; this cap is here so that one forgotten
    /// window is never the only thing a sender is judged on even against a
    /// daemon that does not, and so nothing absurd ever leaves the device.
    static let maximumStintMs: UInt64 = 600_000

    /// The message at the reader's focus, and the time it has earned so far.
    /// `since` is nil while the scene is inactive: the stint is still open,
    /// it is just not earning.
    private struct Stint {
        var messageId: Int
        var banked: UInt64 = 0
        var since: UInt64?
    }

    private var stint: Stint?
    private var dwell: [Int: UInt64] = [:]
    private var clicks: [Int: UInt32] = [:]

    /// The message currently accruing, for the caller's own guards.
    var focused: Int? { stint?.messageId }

    /// `messageId` is at the reader's focus. A different message's stint is
    /// closed first and joins what `drain` hands back; the answer says whether
    /// that happened, so a caller that is asked on every scroll tick only
    /// flushes when there is something new to send.
    ///
    /// Focusing the message already in focus leaves a running stint alone,
    /// and RESTARTS a paused one. The only caller is gated on the scene being
    /// active, so a focus is itself the evidence a `resume` would have been;
    /// treating it as a no-op would let one missed resume stop the clock for
    /// as long as the reader stayed on that message.
    @discardableResult
    mutating func focus(_ messageId: Int, now: UInt64) -> Bool {
        if stint?.messageId == messageId {
            resume(now: now)
            return false
        }
        let closed = blur(now: now)
        stint = Stint(messageId: messageId, since: now)
        return closed
    }

    /// The scene went inactive. Time from here to `resume` does not count.
    mutating func pause(now: UInt64) {
        guard var open = stint, let since = open.since else { return }
        open.banked = Self.sum(open.banked, Self.elapsed(from: since, to: now))
        open.since = nil
        stint = open
    }

    /// The scene is active again. Nothing to do when nothing was paused.
    mutating func resume(now: UInt64) {
        guard stint != nil, stint?.since == nil else { return }
        stint?.since = now
    }

    /// The reader closed, moved to another thread, or put something that is
    /// not received mail at its focus. Says whether a stint was closed.
    @discardableResult
    mutating func blur(now: UInt64) -> Bool {
        guard let open = stint else { return false }
        stint = nil
        var total = open.banked
        if let since = open.since {
            total = Self.sum(total, Self.elapsed(from: since, to: now))
        }
        guard total >= Self.minimumStintMs else { return true }
        dwell[open.messageId] = Self.sum(
            dwell[open.messageId] ?? 0, min(total, Self.maximumStintMs))
        return true
    }

    /// A link in `messageId`'s body was activated. Filed under the message it
    /// was clicked in, which need not be the one at the focus line: a tall
    /// viewport shows the tail of one message above the next.
    mutating func click(_ messageId: Int) {
        let had = clicks[messageId] ?? 0
        clicks[messageId] = had == .max ? had : had + 1
    }

    /// Everything closed and not yet sent, one report per message, and the
    /// clicks so far of a message whose stint is still open: a click usually
    /// takes the user to a browser, and waiting for them to come back and
    /// close the reader is how it would be lost. The open stint's TIME stays
    /// here until it closes.
    mutating func drain() -> [Report] {
        let ids = Set(dwell.keys).union(clicks.keys).sorted()
        let reports = ids.map {
            Report(messageId: $0, dwellMs: dwell[$0] ?? 0, linkClicks: clicks[$0] ?? 0)
        }
        dwell.removeAll()
        clicks.removeAll()
        return reports
    }

    /// Is activating this link a sign the mail was WANTED? A click on the
    /// footer's unsubscribe or preferences link is the opposite, and counting
    /// it would let the act of leaving a list read as engagement with it.
    ///
    /// Only links that say so in their own address are caught. Plenty of
    /// senders route every link, unsubscribe included, through an opaque
    /// redirect, and those still count: this trims the obvious case, it does
    /// not make "clicked" exact. The address is read here and goes no further.
    static func isEngagement(link: String) -> Bool {
        let folded = link.lowercased()
        return !disengagingMarks.contains { folded.contains($0) }
    }

    private static let disengagingMarks = [
        "unsubscribe", "unsub", "optout", "opt-out", "opt_out",
        "email-preferences", "email_preferences", "manage-preferences",
        "subscription-preferences",
    ]

    /// A `now` behind the start is a clock that went backwards, and is worth
    /// nothing rather than a wrapped UInt64 (which would read as 584 million
    /// years and then be capped into a confident ten minutes).
    private static func elapsed(from start: UInt64, to now: UInt64) -> UInt64 {
        now > start ? now - start : 0
    }

    private static func sum(_ a: UInt64, _ b: UInt64) -> UInt64 {
        let (total, overflow) = a.addingReportingOverflow(b)
        return overflow ? .max : total
    }
}
