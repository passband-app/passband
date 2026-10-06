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

// MARK: - the full list

/// One email a smart done cleared, as the full list shows it. The subject is
/// mail content: it lives with the receipt and nowhere else.
struct SmartDoneItem: Identifiable, Hashable, Sendable {
    var id: Int
    /// The daemon's reason code, kept as sent: a code from a newer daemon
    /// still has a row, under a neutral heading.
    var reason: String
    /// Bare address.
    var sender: String
    var senderName: String?
    var subject: String?
    /// RFC 3339.
    var receivedAt: String?

    /// The name to show: the sender's own, else the address.
    /// The daemon sends an empty string for a sender with no name.
    var displayName: String {
        let name = senderName?.trimmingCharacters(in: .whitespaces) ?? ""
        return name.isEmpty ? sender : name
    }
}

/// The full list's shape: a list to AUDIT, so it is grouped by why (the same
/// order the receipt's lines use), each sender's emails together inside a
/// reason, newest first.
enum SmartDoneList {
    struct Group: Identifiable, Hashable, Sendable {
        /// A known reason code, or `other` for every code this build has no
        /// words for.
        var reason: String
        var heading: String
        var items: [SmartDoneItem]
        var id: String { reason }
    }

    static let other = "other"

    static func groups(_ items: [SmartDoneItem]) -> [Group] {
        var by: [String: [SmartDoneItem]] = [:]
        for item in items {
            let key = SmartDoneCopy.order.contains(item.reason) ? item.reason : other
            by[key, default: []].append(item)
        }
        return (SmartDoneCopy.order + [other]).compactMap { reason in
            guard let rows = by[reason], !rows.isEmpty else { return nil }
            return Group(
                reason: reason, heading: SmartDoneCopy.heading(reason, count: rows.count),
                items: sorted(rows))
        }
    }

    /// Senders together, the sender with the newest email first, and each
    /// sender's own emails newest first. Ties fall back to the address and
    /// the id so the order never shuffles between two draws.
    static func sorted(_ items: [SmartDoneItem]) -> [SmartDoneItem] {
        var newest: [String: Date] = [:]
        for item in items {
            let at = date(item.receivedAt)
            if at > (newest[item.sender] ?? .distantPast) || newest[item.sender] == nil {
                newest[item.sender] = max(at, newest[item.sender] ?? .distantPast)
            }
        }
        return items.sorted { a, b in
            if a.sender != b.sender {
                let na = newest[a.sender] ?? .distantPast
                let nb = newest[b.sender] ?? .distantPast
                return na != nb ? na > nb : a.sender < b.sender
            }
            let da = date(a.receivedAt)
            let db = date(b.receivedAt)
            return da != db ? da > db : a.id > b.id
        }
    }

    /// An undated row sorts as oldest rather than failing the list.
    ///
    /// The daemon sends its stored string, with sub-second digits and a
    /// numeric offset (`2026-09-28T03:09:44.036873+00:00`). The fraction is
    /// cut before parsing rather than trusted to a formatter option: whole
    /// seconds are all a sort needs, and how many digits a formatter accepts
    /// is not something to find out from a list in the wrong order.
    static func date(_ iso: String?) -> Date {
        guard let iso else { return .distantPast }
        let whole = iso.replacing(/\.\d+/, with: "")
        return ISO8601DateFormatter().date(from: whole) ?? .distantPast
    }
}

extension SmartDoneCopy {
    /// A group's heading on the full list: the receipt's own line for that
    /// reason, count first, so the page and the card say one thing.
    static func heading(_ reason: String, count: Int) -> String {
        if let line = lines(byReason: [reason: count]).first, order.contains(reason) {
            return line.spoken
        }
        return count == 1 ? "1 other" : "\(count) others"
    }

    /// Why ONE email was cleared, for its row. A code from a newer daemon
    /// gets a neutral line rather than a guess.
    static func why(_ reason: String) -> String {
        switch reason {
        case "event_passed": "The event already happened"
        case "offer_ended": "The offer ended"
        case "expired": "No longer current"
        case "ignored_sender": "From a sender you never open"
        case "stale_promo": "A promotion you never opened"
        default: "Cleared"
        }
    }

    static let viewList = "View full list"
    static let listTitle = "What smart done cleared"
    static let listPaused = "The undo timer is paused while you look."
    static let noSubject = "(no subject)"
}

/// THE UNDO WINDOW, AS A CLOCK THAT CAN STOP. Looking at the full list must
/// not cost any of the time there is to undo, so the pause is real: the
/// deadline moves by exactly the time spent paused, and the ring is drawn
/// from this, never the other way round.
struct SmartDoneClock: Hashable, Sendable {
    /// Moves with the deadline on resume, so elapsed over total stays what
    /// it was at the pause.
    private(set) var startedAt: Date
    private(set) var expiresAt: Date
    private(set) var pausedAt: Date?

    init(startedAt: Date, lasting seconds: TimeInterval) {
        self.startedAt = startedAt
        expiresAt = startedAt.addingTimeInterval(seconds)
    }

    var isPaused: Bool { pausedAt != nil }

    /// Seconds left at `now`. Frozen while paused.
    func remaining(at now: Date) -> TimeInterval {
        max(0, expiresAt.timeIntervalSince(pausedAt ?? now))
    }

    func isExpired(at now: Date) -> Bool { remaining(at: now) <= 0 }

    /// The share of the window left, 1 down to 0, for the ring.
    func fraction(at now: Date) -> Double {
        let total = expiresAt.timeIntervalSince(startedAt)
        guard total > 0 else { return 0 }
        return min(1, remaining(at: now) / total)
    }

    /// Stop the clock. False, and nothing changes, when it is already
    /// stopped (the first pause stands) or has already run out (there is no
    /// window left to protect, and a pause must not bring one back).
    @discardableResult
    mutating func pause(at now: Date) -> Bool {
        guard pausedAt == nil, !isExpired(at: now) else { return false }
        pausedAt = now
        return true
    }

    /// Start it again: the deadline moves by exactly the time spent paused.
    /// False when it was not paused.
    @discardableResult
    mutating func resume(at now: Date) -> Bool {
        guard let paused = pausedAt else { return false }
        let spent = max(0, now.timeIntervalSince(paused))
        startedAt = startedAt.addingTimeInterval(spent)
        expiresAt = expiresAt.addingTimeInterval(spent)
        pausedAt = nil
        return true
    }
}
