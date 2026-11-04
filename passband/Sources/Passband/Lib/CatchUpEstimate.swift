// HOW LONG A FIRST CATCH-UP HAS LEFT, from what the polls actually saw.
//
// The first-load veil (Views/FirstLoadVeil.swift) says "about 4 minutes left"
// over a newly connected mailbox. The daemon reports only `done` of `total`,
// so the rate is the client's own measure: the emails fetched between the
// oldest and newest of the last few polls, over the time between them. A
// window of polls rather than the last two, so one slow page from Gmail does
// not swing the line from four minutes to forty and back.
//
// HONEST BY CONSTRUCTION. No estimate until two polls a few seconds apart have
// seen the count move; none while it is standing still; and the number only
// changes when a poll lands, so there is no countdown ticking between them
// pretending to know more than the last poll did.
//
// Pure Foundation, so test.sh runs it with no app behind it.

import Foundation

struct CatchUpEstimate: Sendable, Equatable {
    struct Sample: Sendable, Equatable {
        let at: Date
        let done: Int
    }

    /// The polls the rate is taken over. Eight is a minute and a bit at the
    /// foreground cadence: long enough to smooth a slow page, short enough to
    /// follow a real change of pace.
    static let window = 8
    /// The shortest span a rate is believed over. Two polls a moment apart
    /// (a connect and its first pull) say nothing about throughput.
    static let minimumSpan: TimeInterval = 5

    private(set) var samples: [Sample] = []

    /// One poll's count. A count that went BACKWARDS is a new walk (the
    /// daemon restarted its catch-up), and the old samples describe a walk
    /// that no longer exists, so they go.
    mutating func record(done: Int, at now: Date) {
        if let last = samples.last {
            if done < last.done { samples.removeAll() }
            else if now <= last.at { return }
        }
        samples.append(Sample(at: now, done: done))
        if samples.count > Self.window { samples.removeFirst(samples.count - Self.window) }
    }

    mutating func reset() { samples.removeAll() }

    /// Emails per second over the window, or nil while there is nothing
    /// honest to say: too few polls, too short a span, or no movement.
    var rate: Double? {
        guard let first = samples.first, let last = samples.last, samples.count >= 2 else { return nil }
        let span = last.at.timeIntervalSince(first.at)
        let moved = last.done - first.done
        guard span >= Self.minimumSpan, moved > 0 else { return nil }
        return Double(moved) / span
    }

    /// Seconds left for `total`, from the newest poll's count.
    func remaining(total: Int) -> TimeInterval? {
        guard let rate, let last = samples.last else { return nil }
        return Double(max(0, total - last.done)) / rate
    }

    /// The whole line under the bar, the same on the veil and on the board's
    /// banner: "1,510 of 4,200 emails · about 4 minutes left".
    func line(done: Int, total: Int) -> String {
        "\(done.formatted()) of \(total.formatted()) emails · \(Self.phrase(remaining(total: total)))"
    }

    /// The pace half of the line. Rounded to what a person plans around: the
    /// minute under an hour, the half hour past it.
    static func phrase(_ seconds: TimeInterval?) -> String {
        guard let seconds else { return "estimating…" }
        if seconds < 60 { return "less than a minute left" }
        let minutes = Int((seconds / 60).rounded())
        if minutes < 60 { return minutes == 1 ? "about a minute left" : "about \(minutes) minutes left" }
        let halfHours = (seconds / 1800).rounded()
        if halfHours <= 2 { return "about an hour left" }
        let hours = halfHours / 2
        let shown = hours == hours.rounded() ? String(Int(hours)) : String(format: "%.1f", hours)
        return "about \(shown) hours left"
    }
}
