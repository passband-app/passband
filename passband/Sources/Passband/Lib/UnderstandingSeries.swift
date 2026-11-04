// The Understanding section's arithmetic: the relationship ledger turned into
// points on the People chart, the two ranked lists beside it, and the memory
// entries grouped for the list under them. Every sentence the section shows
// about a person is built here, from the ledger's own counts, so what the page
// says and what the numbers are can never be two things. Foundation only, like
// UsageSeries beside it, so it tests with the wire types and no chart.
//
// THE CHART IS A LADDER, not a scatter of two unrelated measures. The ledger
// has two kinds of evidence about a sender: replies (the owner wrote back) and
// reads (Passband watched them open it). Replies are the stronger claim, so
// they own the top half: a sender with any replied thread sits at
// 0.5 + share / 2. A sender with none sits in the bottom half by how much of
// their mail is read, at read fraction / 2. The bands that fall out are the
// regions: answered on top, read in the middle, ignored at the floor, and a
// strip on the left for senders too new to judge.

import Foundation

/// Where a sender lands on the People chart.
enum PeopleRegion: String, CaseIterable, Sendable, Identifiable {
    case answer, read, ignore, arrived
    var id: String { rawValue }

    var title: String {
        switch self {
        case .answer: "People you answer"
        case .read: "Things you read"
        case .ignore: "Things you ignore"
        case .arrived: "Just arrived"
        }
    }
}

/// What a point's height rests on. Drawn as its symbol, so a reply share and a
/// read fraction are never mistaken for one another.
enum PeopleMeasure: Equatable, Sendable {
    /// Replied threads over inbound threads.
    case replies(Double)
    /// The median read fraction, or the opened share when none was measurable.
    case reads(Double)
    /// No replies and no attention rows: nothing to stand on.
    case none
}

struct PeoplePoint: Equatable, Identifiable, Sendable {
    let row: RelationshipRow
    let region: PeopleRegion
    let measure: PeopleMeasure
    /// Mail per month, on the plot's log axis; for a just-arrived sender a
    /// position inside the left strip, which is not a rate.
    let x: Double
    /// The ladder, 0...1.
    let y: Double
    /// One sentence, from the numbers.
    let sentence: String
    var id: String { row.addr }
    var name: String { UnderstandingText.name(row) }
}

/// One bar in "Your people".
struct PersonBar: Equatable, Identifiable, Sendable {
    let row: RelationshipRow
    /// 0...1: reply share on the answered tab, fatigue on the fading one.
    let value: Double
    let sentence: String
    var id: String { row.addr }
    var name: String { UnderstandingText.name(row) }
}

struct UnderstandingSeries: Equatable {
    /// How new a sender must be to sit in the left strip: first mail inside
    /// the last two weeks, the same floor the ledger keeps for attention.
    static let arrivedDays = 14
    /// The ladder's two lines. Above `answerLine` the owner replies; between
    /// the two they read; under `ignoreLine` (a read share under 40%) they do
    /// not.
    static let answerLine = 0.5
    static let ignoreLine = 0.2
    /// The plot's x range, in mail per month. Anything rarer than once a
    /// month is drawn at once a month; the strip to the left of `axisFloor`
    /// is the just-arrived band.
    static let stripFloor = 0.3
    static let axisFloor = 1.0
    static let barCount = 8

    let coverageDays: Int
    let points: [PeoplePoint]
    /// Top `barCount` by reply strength.
    let answered: [PersonBar]
    /// Top `barCount` by fatigue.
    let fading: [PersonBar]
    /// The plot's x domain, strip included.
    let xDomain: ClosedRange<Double>

    init(_ response: RelationshipsResponse, now: Date = Date()) {
        coverageDays = response.coverage_days
        let arrivedCut = now.addingTimeInterval(-Double(Self.arrivedDays) * 86_400)

        // The strip's senders, oldest first, spread across it so two new
        // senders with the same count do not stack into one dot.
        // ONLY PEOPLE WHO WRITE: the ledger also has a row for everyone the
        // owner wrote to, and someone who has never sent anything has no mail
        // a month to place and no reads to rank.
        let drawn = response.rows.filter { $0.received_count > 0 }
        let arrivedRows = drawn.filter { Self.isArrived($0, cut: arrivedCut) }
            .sorted { (UnderstandingText.date($0.received_first_at) ?? now) < (UnderstandingText.date($1.received_first_at) ?? now) }
        var stripX: [String: Double] = [:]
        let lo = log(Self.stripFloor * 1.15), hi = log(Self.axisFloor * 0.85)
        for (i, row) in arrivedRows.enumerated() {
            let t = (Double(i) + 0.5) / Double(max(arrivedRows.count, 1))
            stripX[row.addr] = exp(lo + (hi - lo) * t)
        }

        var points: [PeoplePoint] = []
        for row in drawn {
            let (measure, y) = Self.ladder(row)
            let region: PeopleRegion
            let x: Double
            if let sx = stripX[row.addr] {
                region = .arrived
                x = sx
            } else {
                region = y >= Self.answerLine ? .answer : y >= Self.ignoreLine ? .read : .ignore
                x = max(row.received_per_month ?? Self.axisFloor, Self.axisFloor)
            }
            points.append(
                PeoplePoint(
                    row: row, region: region, measure: measure, x: x, y: y,
                    sentence: UnderstandingText.sentence(row, region: region, now: now)))
        }
        // Biggest first, so a small dot is drawn over a large one and stays
        // hoverable.
        self.points = points.sorted { $0.row.received_count > $1.row.received_count }

        let maxX = points.map(\.x).max() ?? 10
        xDomain = Self.stripFloor...max(maxX * 1.6, 30)

        answered = Self.rankAnswered(response.rows)
        fading = Self.rankFading(response.rows)
    }

    // MARK: - placement

    /// New means a first message inside the window. A row with no first
    /// sighting is not new: it is placed by its rate like any other, at the
    /// once-a-month floor when it has none.
    static func isArrived(_ row: RelationshipRow, cut: Date) -> Bool {
        guard let first = UnderstandingText.date(row.received_first_at) else { return false }
        return first > cut
    }

    /// What it takes to sit in the top band: answered on at least two threads,
    /// and on at least a tenth of them. One reply, or a reply to one thread in
    /// 190, is not a person you answer; that sender is placed by its reads.
    static let answeredMinimum = 2
    static let answeredMinimumShare = 0.1

    static func isAnswered(_ row: RelationshipRow) -> Bool {
        row.replied_threads >= answeredMinimum && replyShare(row) >= answeredMinimumShare
    }

    /// The point's height and what it stands on.
    static func ladder(_ row: RelationshipRow) -> (PeopleMeasure, Double) {
        if isAnswered(row) {
            let share = min(1, Double(row.replied_threads) / Double(row.inbound_threads))
            return (.replies(share), answerLine + share * (1 - answerLine))
        }
        if let read = readShare(row) {
            // Kept off the answer line itself: a sender read to the end and
            // never answered is still a reader's sender.
            return (.reads(read), min(read * answerLine, answerLine - 0.02))
        }
        return (.none, 0.03)
    }

    /// How much of a sender's mail is read: the ledger's median read fraction
    /// when it measured one, else the share of rows opened at all.
    static func readShare(_ row: RelationshipRow) -> Double? {
        if let f = row.median_read_fraction { return max(0, min(1, f)) }
        guard row.attention_rows > 0 else { return nil }
        return min(1, Double(row.opened_count) / Double(row.attention_rows))
    }

    // MARK: - ranking

    /// Fewer inbound threads than this and a reply share is an anecdote: one
    /// answer to one email is 100%, and it would lead the list.
    static let answeredFloor = 3

    /// The share the bar shows: replied over inbound.
    static func replyShare(_ row: RelationshipRow) -> Double {
        row.inbound_threads > 0 ? min(1, Double(row.replied_threads) / Double(row.inbound_threads)) : 0
    }

    /// Ranked by the share the bar shows, so the list reads top-down in the
    /// order of its own percentages; ties go to the sender answered more often.
    static func rankAnswered(_ rows: [RelationshipRow]) -> [PersonBar] {
        rows.filter { $0.replied_threads > 0 && $0.inbound_threads >= answeredFloor }
            .sorted { a, b in
                let sa = replyShare(a), sb = replyShare(b)
                return sa != sb ? sa > sb : a.replied_threads > b.replied_threads
            }
            .prefix(barCount)
            .map { row in
                PersonBar(row: row, value: replyShare(row), sentence: UnderstandingText.replies(row))
            }
    }

    static func rankFading(_ rows: [RelationshipRow]) -> [PersonBar] {
        rows.filter { ($0.fatigue_score ?? 0) > 0 }
            .sorted { a, b in
                let fa = a.fatigue_score ?? 0, fb = b.fatigue_score ?? 0
                return fa != fb ? fa > fb : a.received_count > b.received_count
            }
            .prefix(barCount)
            .map { row in
                PersonBar(
                    row: row, value: min(1, row.fatigue_score ?? 0),
                    sentence: UnderstandingText.fatigue(row))
            }
    }

    // MARK: - lookups

    /// The point nearest a pointer, given each point's position on screen.
    /// Nil when nothing is within `radius`.
    static func nearest(
        _ points: [PeoplePoint], to target: (x: Double, y: Double), radius: Double,
        position: (PeoplePoint) -> (x: Double, y: Double)?
    ) -> PeoplePoint? {
        var best: (PeoplePoint, Double)?
        for p in points {
            guard let at = position(p) else { continue }
            let d = hypot(at.x - target.x, at.y - target.y)
            if d <= radius, d < (best?.1 ?? .infinity) { best = (p, d) }
        }
        return best?.0
    }
}

// MARK: - memory

/// The memory entries as the list draws them: one group per kind, in a fixed
/// order, live entries before withdrawn ones.
struct MemoryGroup: Equatable, Identifiable {
    let kind: MemoryKind
    let entries: [MemoryEntry]
    var id: MemoryKind { kind }
    var title: String { UnderstandingText.kindTitle(kind) }
}

enum MemoryGrouping {
    /// The order the list reads in: who the owner is, what is live, who
    /// matters, then habits.
    static let order: [MemoryKind] = [.role, .project, .person, .prefers, .ignores, .other]

    /// Under this many days of ledger the list shows its empty state.
    static let minimumCoverageDays = 14

    static func groups(_ entries: [MemoryEntry]) -> [MemoryGroup] {
        let byKind = Dictionary(grouping: entries, by: \.kind)
        return order.compactMap { kind in
            guard let list = byKind[kind], !list.isEmpty else { return nil }
            return MemoryGroup(kind: kind, entries: list.sorted(by: before))
        }
    }

    /// Live before retired; the owner's own words before the pass's guesses;
    /// then the surer guess first; then id, so the order is stable.
    static func before(_ a: MemoryEntry, _ b: MemoryEntry) -> Bool {
        if a.isRetired != b.isRetired { return !a.isRetired }
        if a.origin != b.origin { return a.origin == .stated }
        if a.confidence != b.confidence { return a.confidence > b.confidence }
        return a.id < b.id
    }

    /// Whether the list leads with its "still learning" note. The daemon's
    /// `enabled` is exactly this (`coverage_days >= 14`); the coverage check
    /// stands in for a daemon that leaves it out. The note sits OVER the
    /// entries, never instead of them: an owner's stated entry is theirs from
    /// day one.
    static func stillLearning(_ response: MemoryResponse) -> Bool {
        !response.enabled || response.coverage_days < minimumCoverageDays
    }
}

// MARK: - words

/// Every sentence the section says, from the numbers. Hand-rolled for the
/// same reason UsageText is: a test reads the same on every locale. No em or
/// en dashes, anywhere: these are user-visible strings.
enum UnderstandingText {
    /// "Sam Rivera", or the bare address when the ledger has no name.
    static func name(_ row: RelationshipRow) -> String {
        if let n = row.display_name?.trimmingCharacters(in: .whitespaces), !n.isEmpty { return n }
        return row.addr
    }

    /// "Name <addr>", the shape the contact card opens on.
    static func senderString(_ row: RelationshipRow) -> String {
        if let n = row.display_name?.trimmingCharacters(in: .whitespaces), !n.isEmpty {
            return "\(n) <\(row.addr)>"
        }
        return row.addr
    }

    /// The median time to a reply, in words. Nil under the ledger's floor of
    /// three replied threads, when the daemon sends no median.
    static func replyTime(_ ms: Int?) -> String? {
        guard let ms, ms >= 0 else { return nil }
        let minutes = Double(ms) / 60_000
        switch minutes {
        case ..<15: return "within minutes"
        case ..<60: return "within the hour"
        case ..<(4 * 60): return "within a few hours"
        case ..<(24 * 60): return "the same day"
        case ..<(3 * 24 * 60): return "within a day or two"
        case ..<(7 * 24 * 60): return "within the week"
        default: return "after a week or more"
        }
    }

    /// "replied to 31 of 33, usually within the hour".
    static func replies(_ row: RelationshipRow) -> String {
        var s = "replied to \(UsageText.count(row.replied_threads)) of \(UsageText.count(row.inbound_threads))"
        if let t = replyTime(row.median_reply_ms) { s += ", usually \(t)" }
        return s
    }

    /// "opened 2 of 38", with how far in when there is a read fraction worth
    /// saying.
    static func reads(_ row: RelationshipRow) -> String {
        guard row.attention_rows > 0 else { return "no reads measured yet" }
        var s = "opened \(UsageText.count(row.opened_count)) of \(UsageText.count(row.attention_rows))"
        if row.opened_count > 0, let f = row.median_read_fraction {
            switch f {
            case 0.85...: s += ", usually to the end"
            case 0.55..<0.85: s += ", most of the way"
            case 0.3..<0.55: s += ", about half"
            default: s += ", a glance"
            }
        }
        return s
    }

    /// The fading tab's line: the fatigue module's own receipt when it gave
    /// one that is a count, in that order of strength (`rarely_opened`,
    /// `cleared_unopened`, `never_clicked`), else the ledger's opened tally.
    static func fatigue(_ row: RelationshipRow) -> String {
        let byCode = Dictionary(row.fatigue_reasons.map { ($0.code, $0) }, uniquingKeysWith: { a, _ in a })
        if let r = byCode["rarely_opened"], let opened = r.int("opened"), let received = r.int("received"),
            received > 0
        {
            return "opened \(UsageText.count(opened)) of \(UsageText.count(received))"
        }
        if let n = byCode["cleared_unopened"]?.int("count"), n > 0 {
            return "cleared \(UsageText.count(n)) without opening"
        }
        if let opened = byCode["never_clicked"]?.int("opened"), opened > 0 {
            return "opened \(UsageText.count(opened)), never clicked a link"
        }
        if row.attention_rows > 0 {
            return "opened \(UsageText.count(row.opened_count)) of \(UsageText.count(row.attention_rows))"
        }
        return "\(UsageText.count(row.received_count)) received, none answered"
    }

    /// The chart's hover line.
    static func sentence(_ row: RelationshipRow, region: PeopleRegion, now: Date) -> String {
        var s: String
        if region == .arrived {
            let first = date(row.received_first_at).map { age($0, now: now, past: "first wrote") }
                ?? "new"
            s = "\(first), \(UsageText.count(row.received_count)) so far"
            if row.replied_threads > 0 { s += ", " + replies(row) }
        } else if UnderstandingSeries.isAnswered(row) {
            s = replies(row)
        } else {
            // Placed by its reads, so it leads with them; a stray reply is
            // still said, after.
            s = reads(row)
            if row.replied_threads > 0, row.inbound_threads > 0 {
                s += ", replied to \(UsageText.count(row.replied_threads)) of \(UsageText.count(row.inbound_threads))"
            }
        }
        if row.corrections > 0 {
            s += row.corrections == 1 ? ", sorting corrected once" : ", sorting corrected \(UsageText.count(row.corrections)) times"
        }
        return s
    }

    /// "first wrote 3 days ago" / "first wrote today".
    static func age(_ then: Date, now: Date, past verb: String) -> String {
        let days = Int((now.timeIntervalSince(then) / 86_400).rounded(.down))
        switch days {
        case ..<1: return "\(verb) today"
        case 1: return "\(verb) yesterday"
        default: return "\(verb) \(days) days ago"
        }
    }

    /// An entry's age off its last affirmation: "today", "3 days ago",
    /// "2 weeks ago", "4 months ago".
    static func entryAge(_ entry: MemoryEntry, now: Date) -> String? {
        guard let then = date(entry.affirmed_at ?? entry.updated_at ?? entry.created_at) else { return nil }
        let days = Int((now.timeIntervalSince(then) / 86_400).rounded(.down))
        switch days {
        case ..<1: return "today"
        case 1: return "yesterday"
        case 2..<14: return "\(days) days ago"
        case 14..<60: return "\(days / 7) weeks ago"
        case 60..<730: return "\(days / 30) months ago"
        default: return "\(days / 365) years ago"
        }
    }

    /// "because of 12 messages" / "because of 2 senders"; nil when the entry
    /// carries no receipts at all.
    static func because(_ entry: MemoryEntry) -> String? {
        let n = entry.provenance.message_ids.count
        if n > 0 { return n == 1 ? "because of 1 message" : "because of \(UsageText.count(n)) messages" }
        let a = entry.provenance.addrs.count
        if a > 0 { return a == 1 ? "because of 1 sender" : "because of \(UsageText.count(a)) senders" }
        return nil
    }

    static func origin(_ origin: MemoryOrigin) -> String {
        switch origin {
        case .stated: "you said"
        case .inferred: "inferred"
        }
    }

    static func kindTitle(_ kind: MemoryKind) -> String {
        switch kind {
        case .role: "Your work"
        case .project: "Right now"
        case .person: "People"
        case .prefers: "How you read"
        case .ignores: "What you skip"
        case .other: "Also noticed"
        }
    }

    /// The empty state, under two weeks of ledger.
    static func stillLearning(days: Int) -> String {
        let span = days == 1 ? "1 day" : "\(max(days, 0)) days"
        return "Still learning. The ledger behind this is \(span) old, and Passband waits for two weeks of mail before it writes anything down about you."
    }

    /// The people chart's note when the attention half is thin.
    static func coverageNote(days: Int) -> String {
        days < UnderstandingSeries.arrivedDays
            ? "\(days == 1 ? "1 day" : "\(max(days, 0)) days") of reading measured, so reads are thin"
            : "over \(UsageText.count(days)) days"
    }

    /// The x axis: "1", "10", "100" a month.
    static func perMonth(_ v: Double) -> String {
        v >= 10 ? UsageText.count(Int(v.rounded())) : String(format: "%.1f", v).replacingOccurrences(of: ".0", with: "")
    }

    // MARK: dates

    nonisolated(unsafe) private static let iso: ISO8601DateFormatter = {
        let f = ISO8601DateFormatter()
        f.formatOptions = [.withInternetDateTime]
        return f
    }()
    nonisolated(unsafe) private static let isoFractional: ISO8601DateFormatter = {
        let f = ISO8601DateFormatter()
        f.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return f
    }()

    static func date(_ s: String?) -> Date? {
        guard let s, !s.isEmpty else { return nil }
        return iso.date(from: s) ?? isoFractional.date(from: s)
    }

    static func rfc3339(_ d: Date) -> String { iso.string(from: d) }
}
