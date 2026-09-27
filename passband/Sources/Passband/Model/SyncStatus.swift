// ONE ANSWER TO "IS PASSBAND WORKING", derived from everything the client knows.
//
// Hosted tenants sat for days in a Gmail rate-limit loop while this app said
// "synced just now" over an empty inbox and "You're all clear." in its biggest
// type. The daemon knew; the pieces were scattered across `gmail`, `catch_up`,
// the client's own poll and nothing at all. This file folds them into one value
// with a fixed precedence so every surface (the banner, the status strip, the
// hero, the phone's footnote) tells the same story.
//
// PURE: Foundation, the wire types and a date parser passed in, so the suite in
// Tests/SyncStatusTests.swift builds with swiftc alone.
//
// Copy rules: no em dashes, say what the human can expect or do rather than the
// mechanism, and never put a raw reason code in a sentence. The code is
// `rawReason`, for hover text and developer mode only.

import Foundation

enum SyncStatus: Equatable, Sendable {
    /// The client cannot reach the daemon at all.
    case unreachable
    /// The Gmail credential is dead. `GmailDisconnectedBanner` owns this one.
    case disconnected
    /// Gmail is rate-limiting this mailbox.
    case throttled(nextTry: Date?, reason: String, waiting: Int)
    /// Any other Gmail failure: `kind` is `http`, `network` or `auth`.
    case gmailError(kind: String, reason: String, nextTry: Date?, waiting: Int)
    /// The daemon is re-walking the mailbox; triage is queued behind it.
    case catchingUp(done: Int, total: Int)
    /// No poll has succeeded yet, so triage has not started.
    case waitingForSync(waiting: Int)
    /// The model provider failed; triage is holding off until `until`.
    case coolingDown(until: Date?, waiting: Int)
    /// Today's triage budget is spent; `parked` jobs resume at `until`.
    case budgetPaused(until: Date?, parked: Int)
    /// Nothing is wrong that the daemon can see (or it is too old to say).
    case healthy

    /// Fold the client's knowledge into one status. Precedence: daemon
    /// unreachable > Gmail sign-in dead > Gmail throttling > other Gmail error >
    /// catch-up > triage blocked (waiting for sync, cooling down, budget) >
    /// healthy. Each outranks the next because it is the REASON the next one
    /// cannot move.
    static func derive(
        daemonDown: Bool, stats: StoreStats?, now: Date = Date(),
        date: (String?) -> Date?
    ) -> SyncStatus {
        if daemonDown { return .unreachable }
        guard let stats else { return .healthy }
        if stats.gmail?.connected == false { return .disconnected }
        let waiting = max(stats.triage?.queued ?? 0, 0) + max(stats.triage?.leased ?? 0, 0)
        if let sync = stats.sync, !sync.ok, let error = sync.error {
            let next = date(sync.next_retry_at)
            if error.kind == "quota" {
                return .throttled(nextTry: next, reason: error.reason, waiting: waiting)
            }
            return .gmailError(
                kind: error.kind, reason: error.reason, nextTry: next, waiting: waiting)
        }
        if let c = stats.catch_up, c.total > 0 {
            return .catchingUp(done: c.done, total: c.total)
        }
        if let triage = stats.triage {
            // Only when there is work waiting: a daemon that has just started
            // and has nothing queued is not blocking anything, and a banner for
            // the few seconds before its first poll would be noise.
            if triage.waiting_for_sync && waiting > 0 {
                return .waitingForSync(waiting: waiting)
            }
            if let until = date(triage.cooldown_until), until > now, waiting > 0 {
                return .coolingDown(until: until, waiting: waiting)
            }
            if triage.budget_parked > 0 {
                return .budgetPaused(
                    until: date(triage.budget_resumes_at), parked: triage.budget_parked)
            }
        }
        return .healthy
    }

    // MARK: - copy

    /// The Google reason code, for `.help` hover text and developer mode.
    var rawReason: String? {
        switch self {
        case .throttled(_, let reason, _): return reason
        case .gmailError(let kind, let reason, _, _): return "\(kind): \(reason)"
        default: return nil
        }
    }

    /// Whether this status earns the banner under the masthead. The dead
    /// credential has its own banner, and an unreachable daemon its own pane.
    var showsBanner: Bool {
        switch self {
        case .unreachable, .disconnected, .healthy: return false
        default: return true
        }
    }

    /// Gmail sync itself is not succeeding, so nothing downstream can move.
    var blocksSync: Bool {
        switch self {
        case .unreachable, .disconnected, .throttled, .gmailError, .waitingForSync: return true
        default: return false
        }
    }

    /// True for the states worth a warning tint; the rest are informational.
    var isWarning: Bool {
        switch self {
        case .throttled, .gmailError, .coolingDown, .unreachable, .disconnected: return true
        default: return false
        }
    }

    /// The banner's bold line.
    var headline: String {
        switch self {
        case .unreachable: return "Passband cannot reach its server"
        case .disconnected: return "Passband cannot read your mail"
        case .throttled: return "Gmail is rate-limiting this mailbox"
        case .gmailError(let kind, _, _, _):
            return kind == "network"
                ? "Passband cannot reach Gmail right now"
                : "Gmail is not answering for this mailbox"
        case .catchingUp: return "Catching up on your mailbox"
        case .waitingForSync: return "Triage is waiting for Gmail"
        case .coolingDown: return "Triage is paused for a moment"
        case .budgetPaused: return "Today's triage budget is used up"
        case .healthy: return "Everything is up to date"
        }
    }

    /// The banner's second line, with real numbers and times.
    func detail(now: Date = Date(), calendar: Calendar = .current, locale: Locale = .current)
        -> String
    {
        let at = { (d: Date?) in Self.when(d, now: now, calendar: calendar, locale: locale) }
        switch self {
        case .unreachable:
            return "Mail you already have is still here. New mail appears when it reconnects."
        case .disconnected:
            return "New mail is not arriving until Gmail is reconnected."
        case .throttled(let next, _, let waiting):
            return join(
                "New mail arrives once Gmail lets Passband back in.", retry(at(next)),
                Self.waitingLine(waiting))
        case .gmailError(_, _, let next, let waiting):
            return join(
                "New mail is not arriving yet.", retry(at(next)), Self.waitingLine(waiting))
        case .catchingUp(let done, let total):
            return "\(done.formatted()) of \(total.formatted()) messages fetched. "
                + "Triage starts when it finishes."
        case .waitingForSync(let waiting):
            return "\(Self.emails(waiting)) will be sorted once Gmail sync succeeds."
        case .coolingDown(let until, let waiting):
            let resume = at(until).map { "Passband tries again \($0)." }
            return join(
                "The sorting service had a problem.", resume,
                Self.waitingLine(waiting))
        case .budgetPaused(let until, let parked):
            let when = at(until).map { " \($0)" } ?? " when the budget resets"
            return "\(Self.emails(parked)) will be sorted\(when)."
        case .healthy:
            return ""
        }
    }

    /// One line for tight spots: the phone's footnote, a status chip.
    func oneLine(now: Date = Date(), calendar: Calendar = .current, locale: Locale = .current)
        -> String?
    {
        switch self {
        case .healthy: return nil
        default:
            let d = detail(now: now, calendar: calendar, locale: locale)
            return d.isEmpty ? headline + "." : "\(headline). \(d)"
        }
    }

    /// What the hero says INSTEAD of "You're all clear." when the board is
    /// empty because triage has not run, not because there is nothing to do.
    /// Nil means the empty board is the truth and "all clear" may stand.
    func emptyHero(
        queued: Int, lastSync: Date?, now: Date = Date()
    ) -> (title: String, note: String)? {
        switch self {
        case .unreachable:
            return nil
        case .disconnected:
            return ("Mail is paused.", "New mail cannot arrive until Gmail is reconnected.")
        case .throttled, .gmailError, .waitingForSync:
            if queued > 0 {
                return (
                    "Nothing sorted yet.",
                    "\(Self.emails(queued)) waiting. Triage starts once Gmail sync succeeds."
                )
            }
            let since = lastSync.flatMap { Self.ago($0, now: now) }
            return (
                "Nothing new to show.",
                since.map { "Gmail has not synced since \($0), so newer mail may be waiting." }
                    ?? "Gmail has not synced yet, so your mail may not be here."
            )
        case .catchingUp:
            return ("Nothing sorted yet.", "Triage starts when the catch-up finishes.")
        case .coolingDown:
            guard queued > 0 else { return nil }
            return ("Nothing sorted yet.", "\(Self.emails(queued)) waiting. Triage resumes shortly.")
        case .budgetPaused:
            guard queued > 0 else { return nil }
            return (
                "Nothing sorted yet.",
                "\(Self.emails(queued)) waiting for tomorrow's triage budget."
            )
        case .healthy:
            guard queued > 0 else { return nil }
            return ("Sorting your mail.", "\(Self.emails(queued)) being triaged now.")
        }
    }

    // MARK: - helpers

    static func emails(_ n: Int) -> String {
        n == 1 ? "1 email" : "\(n.formatted()) emails"
    }

    static func waitingLine(_ n: Int) -> String? {
        guard n > 0 else { return nil }
        return n == 1
            ? "1 email is waiting for triage."
            : "\(n.formatted()) emails are waiting for triage."
    }

    private func retry(_ when: String?) -> String? {
        guard let when else { return nil }
        return when == "now" ? "Trying again now." : "Next try \(when)."
    }

    private func join(_ parts: String?...) -> String {
        parts.compactMap { $0 }.joined(separator: " ")
    }

    /// "at 12:05", "tomorrow at 9:00", "on Sep 28 at 9:00", or "now" for a
    /// time already passed. Nil in, nil out.
    static func when(
        _ date: Date?, now: Date, calendar: Calendar, locale: Locale
    ) -> String? {
        guard let date else { return nil }
        if date.timeIntervalSince(now) < 30 { return "now" }
        var style = Date.FormatStyle(date: .omitted, time: .shortened)
        style.calendar = calendar
        style.timeZone = calendar.timeZone
        style.locale = locale
        let time = date.formatted(style)
        if calendar.isDate(date, inSameDayAs: now) { return "at \(time)" }
        if let tomorrow = calendar.date(byAdding: .day, value: 1, to: now),
            calendar.isDate(date, inSameDayAs: tomorrow)
        {
            return "tomorrow at \(time)"
        }
        var day = Date.FormatStyle(date: .abbreviated, time: .omitted)
        day.calendar = calendar
        day.timeZone = calendar.timeZone
        day.locale = locale
        return "on \(date.formatted(day)) at \(time)"
    }

    /// "4 minutes ago" style, for the hero's note.
    static func ago(_ date: Date, now: Date) -> String? {
        let f = RelativeDateTimeFormatter()
        f.unitsStyle = .full
        return f.localizedString(for: date, relativeTo: now)
    }
}
