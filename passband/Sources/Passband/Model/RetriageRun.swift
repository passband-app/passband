// A dev re-triage as the CLIENT sees it: a total, a count, and the three ways
// the wait can end. The daemon's own view of the same run is
// `GET /client/retriage` (see `RetriageProgress`); this is what the blocking
// modal renders, so every state it can be in needs copy, including the two the
// daemon cannot describe — a route it is too old to serve, and a route that
// stopped answering.

import Foundation

struct RetriageRun: Sendable, Equatable {
    /// Rows in the run. Seeded from the kick's own `reset` so the modal opens
    /// with a real number, then replaced by the server's live total.
    var total = 0
    var done = 0
    /// True once a progress poll has answered. Until it does, `done` is a
    /// guess of zero rather than a measurement — the difference matters,
    /// because 0 of 40 and "not counting yet" look identical otherwise.
    var counted = false
    /// The daemon has no `GET /client/retriage`. The re-triage itself is still
    /// running — the reset landed — so this is "we cannot watch it", not
    /// "it failed", and the modal says exactly that.
    var unsupported = false
    /// Polling gave up. Same distinction: the daemon is doing the work or it
    /// is not, and either way we have stopped being able to tell.
    var failure: String?
    /// When `done` last moved, seeded at the kick.
    var lastAdvance = Date()
    /// The counter has not moved in [`stallSeconds`]. NOT an end state — the
    /// poll keeps running and a run that wakes up carries on — it only opens
    /// the way out, because this modal has no other one and a wedged daemon
    /// would otherwise cost the user a force-quit.
    var stalled = false
    /// Jobs the daemon parked on the daily triage budget, and when the first of
    /// them resumes. A parked run is PAUSED, not stuck: it will not move until
    /// the budget resets, which can be hours, so it says why and opens the door
    /// at once instead of making somebody sit out the stall window first.
    var parked = 0
    var resumesAt: String?

    /// Sized on what a human will stare at, NOT on the daemon's cadence — which
    /// this client cannot see and which has already moved once (`sync.poll_secs`
    /// went 45 -> 5), so a threshold derived from it would be wrong on exactly
    /// the installs it was tuned against.
    ///
    /// The asymmetry is what makes that safe: firing early only OPENS A DOOR —
    /// nothing closes, nothing stops, and a run that wakes up clears the flag on
    /// its next poll — while firing late is a person stuck behind a dead counter
    /// with force-quit as the only way out. So err short.
    private static let stallSeconds: TimeInterval = 90

    /// Adopt a poll. The SERVER's total wins over the kick's seed: a second
    /// kick, or a per-message re-triage from the fix palette, is the same run
    /// to the queues and should be the same run here.
    ///
    /// `done` NEVER GOES BACKWARDS on screen. It genuinely can on the wire —
    /// new mail landing mid-run joins the queues, and a row can be re-stamped —
    /// and a counter that walks back reads as a bug in a modal the user is
    /// already trapped behind.
    mutating func adopt(_ p: RetriageProgress, at now: Date = Date()) {
        counted = true
        total = max(p.total, 0)
        let next = min(max(done, p.done), total)
        if next != done { lastAdvance = now }
        done = next
        stalled = now.timeIntervalSince(lastAdvance) > Self.stallSeconds
        parked = max(p.budget_parked ?? 0, 0)
        resumesAt = parked > 0 ? p.budget_resumes_at : nil
        blocked = finished ? nil : p.blocked
    }

    /// What the daemon says is holding the run (Gmail sync failing, a provider
    /// cooldown, a spent budget), or nil. Absent on an older daemon.
    var blocked: RetriageBlocked?

    /// Some of the run is waiting on tomorrow's budget.
    var paused: Bool { counted && parked > 0 && !finished }

    /// The pause in plain words, with the resume time in the viewer's own clock.
    static func pauseNote(
        resumesAt: Date?, now: Date = Date(), calendar: Calendar = .current,
        locale: Locale = .current
    ) -> String {
        let lead = "Paused: today's triage budget is used up."
        guard let resumesAt else { return lead }
        var style = Date.FormatStyle(date: .omitted, time: .shortened)
        style.calendar = calendar
        style.timeZone = calendar.timeZone
        style.locale = locale
        let time = resumesAt.formatted(style)
        if calendar.isDate(resumesAt, inSameDayAs: now) { return "\(lead) Resumes at \(time)." }
        if let tomorrow = calendar.date(byAdding: .day, value: 1, to: now),
            calendar.isDate(resumesAt, inSameDayAs: tomorrow)
        {
            return "\(lead) Resumes tomorrow at \(time)."
        }
        return lead
    }

    /// Nothing left in either queue. Only ever true once a poll has ANSWERED:
    /// the seeded state is `0 done of N`, and an unanswered `0 of 0` must not
    /// read as a finished run.
    var finished: Bool { counted && done >= total }

    /// Whether the wait can still end on its own. False parks the modal on its
    /// close button.
    var watching: Bool { !unsupported && failure == nil }

    /// Whether to offer the way out. A live, moving run does NOT — that is the
    /// whole point of the modal — but a run nobody can watch or that has stopped
    /// moving must never be a window the user cannot get back.
    ///
    /// A BLOCKED run opens it at once too: a run held by a failed Gmail sync can
    /// sit for hours, and the daemon has told us so, so there is nothing to wait
    /// out before offering the door.
    var canClose: Bool { !watching || stalled || paused || blocked != nil }

    /// 0...1 for the bar, and a full bar only for a run that really finished.
    var fraction: Double {
        guard counted, total > 0 else { return 0 }
        return Double(done) / Double(total)
    }
}

// MARK: - the three steps

/// Where a re-triage is, as the three things it waits on in order: Gmail sync,
/// the triage queue, and the count of rows done. A step that is BLOCKED says why
/// and until when, which is what the single bar could never say: "not moving"
/// and "waiting for Gmail until 12:05" were the same screen.
struct RetriageStep: Equatable, Sendable {
    enum State: Equatable, Sendable { case done, active, blocked, waiting }
    var title: String
    var detail: String?
    var state: State
}

extension RetriageRun {
    /// The three steps, from this run, the derived sync status and the stats
    /// triage block. Pure, so the precedence is asserted in the suite.
    func steps(
        sync: SyncStatus, triage: TriageHealth?, now: Date = Date(),
        calendar: Calendar = .current, locale: Locale = .current,
        date: (String?) -> Date?
    ) -> [RetriageStep] {
        let at = { (d: Date?) in SyncStatus.when(d, now: now, calendar: calendar, locale: locale) }
        let retry = { (d: Date?) -> String? in
            at(d).map { $0 == "now" ? "Trying again now." : "Next try \($0)." }
        }
        let joined = { (parts: [String?]) in parts.compactMap { $0 }.joined(separator: " ") }

        // 1. Gmail sync.
        var syncStep = RetriageStep(title: "Gmail sync", detail: "Up to date", state: .done)
        switch sync {
        case .unreachable:
            syncStep = .init(
                title: "Gmail sync", detail: "Passband cannot reach its server.", state: .blocked)
        case .disconnected:
            syncStep = .init(
                title: "Gmail sync", detail: "Reconnect Gmail to continue.", state: .blocked)
        case .throttled(let next, _, _), .gmailError(_, _, let next, _):
            syncStep = .init(
                title: "Gmail sync", detail: joined([sync.headline + ".", retry(next)]),
                state: .blocked)
        case .catchingUp(let done, let total):
            syncStep = .init(
                title: "Gmail sync",
                detail: "Catching up: \(done.formatted()) of \(total.formatted()) messages",
                state: .active)
        case .waitingForSync:
            syncStep = .init(
                title: "Gmail sync",
                detail: joined(["Waiting for Gmail to answer.", retry(date(blocked?.until))]),
                state: .blocked)
        default:
            if blocked?.reason == "waiting_for_sync" {
                syncStep = .init(
                    title: "Gmail sync",
                    detail: joined(["Waiting for Gmail to answer.", retry(date(blocked?.until))]),
                    state: .blocked)
            }
        }
        let syncDone = syncStep.state == .done

        // 2. The triage queue.
        let counts = triage.map {
            "\($0.queued.formatted()) queued, \($0.leased.formatted()) running"
        }
        var queue = RetriageStep(title: "Triage queue", detail: counts, state: .active)
        if finished {
            queue = .init(title: "Triage queue", detail: "Empty", state: .done)
        } else if !syncDone {
            queue = .init(
                title: "Triage queue",
                detail: joined([counts.map { $0 + "." }, "Starts once Gmail sync succeeds."]),
                state: .waiting)
        } else if blocked?.reason == "provider_cooldown" {
            queue = .init(
                title: "Triage queue",
                detail: joined([
                    "The sorting service had a problem.",
                    at(date(blocked?.until)).map { "Resumes \($0)." },
                ]),
                state: .blocked)
        } else if paused || blocked?.reason == "budget" {
            let resume = date(resumesAt ?? blocked?.until)
            queue = .init(
                title: "Triage queue",
                detail: Self.pauseNote(
                    resumesAt: resume, now: now, calendar: calendar, locale: locale),
                state: .blocked)
        }

        // 3. The count.
        let tally = counted ? "\(done.formatted()) of \(total.formatted()) emails" : nil
        let last: RetriageStep.State =
            finished ? .done : (queue.state == .active ? .active : .waiting)
        return [
            syncStep, queue, RetriageStep(title: "Re-triaged", detail: tally, state: last),
        ]
    }
}
