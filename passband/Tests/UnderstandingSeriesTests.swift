// The Understanding section's arithmetic, pinned. Every sentence the page says
// about a person is a claim built from the ledger's counts, and every way it
// can be wrong is invisible until somebody reads it about their own cofounder:
// a recruiter answered once ranked over the person answered 31 times, a
// newsletter nobody opens drawn among the people, a reply time that says
// "within minutes" for a day. So the placements, the rankings, the grouping
// and the words are asserted here, against the shipped fixture and against
// rows built to sit on each line.

import Foundation

@main
@MainActor
struct UnderstandingSeriesTests {
    static var failures = 0
    static var checks = 0

    static let now = Date(timeIntervalSince1970: 1_793_000_000)

    static func main() {
        regionsFollowTheLadder()
        newSendersSitInTheStripWhateverTheyDo()
        repliesOutrankReadsAndTheStripStaysLeft()
        answeredRanksByStrengthNotByShare()
        fadingRanksByFatigueAndQuotesItsReceipt()
        replyTimeBucketsInWords()
        sentencesComeFromTheNumbers()
        memoryGroupsInOrderLiveFirst()
        memoryWordsAndTheEmptyState()
        nearestPicksTheClosestWithinReach()
        noDashesInAnythingTheSectionSays()

        if failures > 0 {
            print("FAILED: \(failures) of \(checks) checks")
            exit(1)
        }
        print("ok: \(checks) checks passed")
    }

    // MARK: - fixtures

    static func ago(_ days: Double) -> String { UnderstandingText.rfc3339(now.addingTimeInterval(-days * 86_400)) }

    static func row(
        _ addr: String, perMonth: Double? = 10, firstDaysAgo: Double = 200, inbound: Int = 0, replied: Int = 0,
        replyMs: Int? = nil, attention: Int = 0, opened: Int = 0, read: Double? = nil, corrections: Int = 0,
        fatigue: Double? = nil, reasons: [FatigueReason] = [], received: Int = 10, name: String? = nil
    ) -> RelationshipRow {
        RelationshipRow(
            addr: addr, display_name: name, received_count: received, received_per_month: perMonth,
            received_first_at: ago(firstDaysAgo), inbound_threads: inbound, replied_threads: replied,
            median_reply_ms: replyMs, attention_rows: attention, opened_count: opened, median_read_fraction: read,
            corrections: corrections, fatigue_score: fatigue, fatigue_reasons: reasons)
    }

    static func series(_ rows: [RelationshipRow], coverage: Int = 41) -> UnderstandingSeries {
        UnderstandingSeries(RelationshipsResponse(coverage_days: coverage, computed_at: nil, rows: rows), now: now)
    }

    static func point(_ s: UnderstandingSeries, _ addr: String) -> PeoplePoint? {
        s.points.first { $0.id == addr }
    }

    // MARK: - placement

    static func regionsFollowTheLadder() {
        let s = series([
            row("answered", inbound: 10, replied: 9),
            row("once", inbound: 6, replied: 1),
            row("reader", attention: 20, opened: 18, read: 0.9),
            row("skimmer", attention: 20, opened: 12, read: 0.3),
            row("ignored", attention: 38, opened: 2, read: 0.1),
            row("opened-only", attention: 10, opened: 6),
            row("unmeasured"),
        ])
        expect(point(s, "answered")?.region == .answer, "a sender answered 9 of 10 is someone you answer")
        expect(point(s, "once")?.region == .answer, "one reply is still a reply: the top half")
        expect(point(s, "reader")?.region == .read, "read to the end, never answered: things you read")
        expect(point(s, "skimmer")?.region == .ignore, "a 30% read is under the 40% line")
        expect(point(s, "ignored")?.region == .ignore, "opened 2 of 38 is ignored")
        expect(point(s, "opened-only")?.region == .read, "with no read fraction the opened share stands in (6 of 10)")
        expect(point(s, "unmeasured")?.region == .ignore && point(s, "unmeasured")?.measure == PeopleMeasure.none,
            "nothing to stand on sits at the floor, marked as unmeasured")

        let a = point(s, "answered")!, once = point(s, "once")!
        expect(abs(a.y - 0.95) < 1e-9, "the top half is 0.5 + share / 2 (9 of 10 is 0.95)")
        expect(abs(once.y - (0.5 + 0.5 / 6)) < 1e-9, "1 of 6 sits just over the line")
        expect(a.measure == .replies(0.9), "a replied sender's height rests on replies")
        let reader = point(s, "reader")!
        expect(reader.measure == .reads(0.9) && reader.y < UnderstandingSeries.answerLine,
            "a reader never crosses into the answered half, however far they read")
    }

    static func newSendersSitInTheStripWhateverTheyDo() {
        let s = series([
            row("new-answered", perMonth: nil, firstDaysAgo: 3, inbound: 1, replied: 1, received: 2),
            row("new-ignored", perMonth: nil, firstDaysAgo: 6, attention: 2, received: 3),
            row("no-sighting", perMonth: nil, firstDaysAgo: 0).withFirst(nil),
            row("old", perMonth: 4, firstDaysAgo: 30),
        ])
        expect(point(s, "new-answered")?.region == .arrived, "first mail 3 days ago is just arrived, even answered")
        expect(point(s, "new-ignored")?.region == .arrived, "first mail 6 days ago is just arrived")
        expect(point(s, "no-sighting")?.region == .arrived, "no first sighting and no rate cannot be placed: the strip")
        expect(point(s, "old")?.region != .arrived, "30 days old is placed by what it does")
        let strip = s.points.filter { $0.region == .arrived }.map(\.x)
        expect(strip.allSatisfy { $0 > UnderstandingSeries.stripFloor && $0 < UnderstandingSeries.axisFloor },
            "every strip point is inside the strip")
        expect(Set(strip).count == strip.count, "two new senders never share an x")
    }

    static func repliesOutrankReadsAndTheStripStaysLeft() {
        let s = series([
            row("rare", perMonth: 0.2, inbound: 2, replied: 2),
            row("busy", perMonth: 190, attention: 10, opened: 0),
        ])
        expect(point(s, "rare")?.x == UnderstandingSeries.axisFloor, "rarer than once a month is drawn at once a month")
        expect(s.xDomain.lowerBound == UnderstandingSeries.stripFloor, "the domain starts at the strip")
        expect(s.xDomain.upperBound >= 190 * 1.5, "the domain clears the busiest sender")
        expect(s.points.first?.row.received_count ?? 0 >= s.points.last?.row.received_count ?? 0,
            "biggest first, so small dots draw on top")
    }

    // MARK: - ranking

    static func answeredRanksByStrengthNotByShare() {
        let s = series([
            row("one-of-one", inbound: 1, replied: 1),
            row("cofounder", inbound: 33, replied: 31, replyMs: 22 * 60_000),
            row("lawyer", inbound: 9, replied: 8),
            row("never", attention: 5, opened: 5, read: 1),
        ])
        expect(s.answered.map(\.id) == ["cofounder", "lawyer", "one-of-one"],
            "31 of 33 outranks 8 of 9 outranks 1 of 1; a sender never answered is not listed")
        expect(s.answered.first?.value == 31.0 / 33.0, "the bar is the real share")
        expect(s.answered.first?.sentence == "replied to 31 of 33, usually within the hour", "and says it")

        let many = series((0..<12).map { row("p\($0)", inbound: 10, replied: $0 % 10 + 1) })
        expect(many.answered.count == UnderstandingSeries.barCount, "at most eight bars")
    }

    static func fadingRanksByFatigueAndQuotesItsReceipt() {
        let s = series([
            row("a", attention: 38, opened: 2, fatigue: 0.86,
                reasons: [FatigueReason(code: "opened_few", counts: ["opened": 2, "of": 38])]),
            row("b", attention: 23, opened: 1, fatigue: 0.92),
            row("c", fatigue: 0.4, received: 7),
            row("fine", fatigue: 0),
            row("unscored"),
        ])
        expect(s.fading.map(\.id) == ["b", "a", "c"], "by fatigue, highest first, unscored and zero left out")
        expect(s.fading[1].sentence == "opened 2 of 38", "the module's own receipt when it gave one")
        expect(s.fading[0].sentence == "opened 1 of 23", "else the ledger's opened tally")
        expect(s.fading[2].sentence == "7 received, none answered", "else what is left to say")
    }

    // MARK: - words

    static func replyTimeBucketsInWords() {
        let m = 60_000
        let cases: [(Int?, String?)] = [
            (nil, nil), (-1, nil), (5 * m, "within minutes"), (22 * m, "within the hour"),
            (3 * 60 * m, "within a few hours"), (9 * 60 * m, "the same day"),
            (30 * 60 * m, "within a day or two"), (5 * 24 * 60 * m, "within the week"),
            (10 * 24 * 60 * m, "after a week or more"),
        ]
        for (ms, want) in cases {
            expect(UnderstandingText.replyTime(ms) == want, "reply time \(String(describing: ms)) reads \(want ?? "nil")")
        }
        expect(UnderstandingText.replyTime(15 * m) == "within the hour", "15 minutes is the hour bucket's floor")
    }

    static func sentencesComeFromTheNumbers() {
        let s = series([
            row("vendor", attention: 30, opened: 14, read: 0.48, corrections: 4),
            row("once", attention: 1, opened: 1, read: 1, corrections: 1),
            row("new", perMonth: nil, firstDaysAgo: 3, inbound: 1, replied: 1, received: 2),
            row("today", perMonth: nil, firstDaysAgo: 0.2, received: 1),
            row("ignored", attention: 38, opened: 2, read: 0.1),
            row("none", attention: 0),
        ])
        expect(point(s, "vendor")?.sentence == "opened 14 of 30, about half, sorting corrected 4 times", "a corrected vendor")
        expect(point(s, "once")?.sentence == "opened 1 of 1, usually to the end, sorting corrected once", "one correction")
        expect(point(s, "new")?.sentence == "first wrote 3 days ago, 2 so far, replied to 1 of 1", "a new sender")
        expect(point(s, "today")?.sentence == "first wrote today, 1 so far", "today")
        expect(point(s, "ignored")?.sentence == "opened 2 of 38, a glance", "a glance")
        expect(point(s, "none")?.sentence == "no reads measured yet", "nothing measured says so")

        let named = row("sam@x.example", name: "  Sam Rivera ")
        expect(UnderstandingText.name(named) == "Sam Rivera", "the name, trimmed")
        expect(UnderstandingText.senderString(named) == "Sam Rivera <sam@x.example>", "the contact card's string")
        expect(UnderstandingText.name(row("bare@x.example", name: "")) == "bare@x.example", "no name, the address")
        expect(UnderstandingText.senderString(row("bare@x.example")) == "bare@x.example", "and the bare string")
        expect(UnderstandingText.perMonth(1) == "1" && UnderstandingText.perMonth(3) == "3"
            && UnderstandingText.perMonth(0.5) == "0.5" && UnderstandingText.perMonth(100) == "100",
            "axis labels")
    }

    // MARK: - memory

    static func entry(
        _ id: String, _ kind: MemoryKind, _ origin: MemoryOrigin = .inferred, confidence: Double = 0.5,
        retired: Bool = false, messages: [Int] = [], addrs: [String] = [], affirmedDaysAgo: Double = 3
    ) -> MemoryEntry {
        MemoryEntry(
            id: id, origin: origin, kind: kind, line: "line \(id)",
            provenance: MemoryProvenance(message_ids: messages, addrs: addrs), confidence: confidence,
            affirmed_at: ago(affirmedDaysAgo), retired_at: retired ? ago(1) : nil)
    }

    static func memoryGroupsInOrderLiveFirst() {
        let groups = MemoryGrouping.groups([
            entry("ign", .ignores),
            entry("p-retired", .person, retired: true),
            entry("p-low", .person, confidence: 0.3),
            entry("p-stated", .person, .stated, confidence: 0.1),
            entry("p-high", .person, confidence: 0.9),
            entry("role", .role),
            entry("odd", .other),
        ])
        expect(groups.map(\.kind) == [.role, .person, .ignores, .other], "fixed kind order, empty kinds left out")
        expect(groups[1].entries.map(\.id) == ["p-stated", "p-high", "p-low", "p-retired"],
            "stated first, then by confidence, the withdrawn last")
        expect(groups[1].title == "People" && groups[3].title == "Also noticed", "kind titles")
    }

    static func memoryWordsAndTheEmptyState() {
        expect(UnderstandingText.because(entry("a", .role, messages: [1])) == "because of 1 message", "one message")
        expect(UnderstandingText.because(entry("a", .role, messages: [1, 2, 3])) == "because of 3 messages", "messages")
        expect(UnderstandingText.because(entry("a", .role, addrs: ["x", "y"])) == "because of 2 senders", "senders when no messages")
        expect(UnderstandingText.because(entry("a", .role)) == nil, "no receipts, no affordance")
        expect(UnderstandingText.origin(.stated) == "you said" && UnderstandingText.origin(.inferred) == "inferred", "origin badges")
        expect(UnderstandingText.entryAge(entry("a", .role, affirmedDaysAgo: 0.2), now: now) == "today", "today")
        expect(UnderstandingText.entryAge(entry("a", .role, affirmedDaysAgo: 1.5), now: now) == "yesterday", "yesterday")
        expect(UnderstandingText.entryAge(entry("a", .role, affirmedDaysAgo: 5), now: now) == "5 days ago", "days")
        expect(UnderstandingText.entryAge(entry("a", .role, affirmedDaysAgo: 21), now: now) == "3 weeks ago", "weeks")
        expect(UnderstandingText.entryAge(entry("a", .role, affirmedDaysAgo: 95), now: now) == "3 months ago", "months")

        expect(MemoryGrouping.stillLearning(MemoryResponse(enabled: true, coverage_days: 13, entries: [])),
            "13 days of ledger is still learning")
        expect(!MemoryGrouping.stillLearning(MemoryResponse(enabled: true, coverage_days: 14, entries: [])),
            "two weeks is enough")
        expect(MemoryGrouping.stillLearning(MemoryResponse(enabled: false, coverage_days: 9, entries: [])),
            "the daemon's enabled = false is still learning")
        expect(MemoryGrouping.stillLearning(MemoryResponse(enabled: false, coverage_days: 30, entries: [])),
            "and it wins over a coverage count that disagrees")
        expect(UnderstandingText.stillLearning(days: 1).contains("1 day old"), "singular day")
        expect(UnderstandingText.stillLearning(days: 9).contains("9 days old"), "plural days")
    }

    static func nearestPicksTheClosestWithinReach() {
        let s = series([row("a", inbound: 2, replied: 2), row("b", attention: 2, opened: 0)])
        let at: (PeoplePoint) -> (x: Double, y: Double)? = { p in p.id == "a" ? (100, 100) : (130, 100) }
        expect(UnderstandingSeries.nearest(s.points, to: (110, 100), radius: 24, position: at)?.id == "a", "closest wins")
        expect(UnderstandingSeries.nearest(s.points, to: (124, 100), radius: 24, position: at)?.id == "b", "the other side")
        expect(UnderstandingSeries.nearest(s.points, to: (300, 300), radius: 24, position: at) == nil, "out of reach is nothing")
    }

    static func noDashesInAnythingTheSectionSays() {
        let fixture = UnderstandingSeries(UnderstandingFixture.relationships(now: now), now: now)
        let memory = UnderstandingFixture.memory(now: now)
        var said: [String] = fixture.points.map(\.sentence) + fixture.answered.map(\.sentence)
            + fixture.fading.map(\.sentence) + PeopleRegion.allCases.map(\.title)
            + memory.entries.map(\.line) + MemoryKind.allCases.map(UnderstandingText.kindTitle)
            + [UnderstandingText.stillLearning(days: 3), UnderstandingText.coverageNote(days: 3),
               UnderstandingText.coverageNote(days: 41)]
        said += memory.entries.compactMap(UnderstandingText.because)
        for s in said {
            expect(!s.contains("\u{2014}") && !s.contains("\u{2013}"), "no em or en dash in \"\(s)\"")
        }

        // The fixture exercises every region, so the demo shows all four.
        expect(Set(fixture.points.map(\.region)) == Set(PeopleRegion.allCases), "the fixture fills every region")
        expect(fixture.answered.first?.row.display_name == "Sam Rivera", "the cofounder leads the answered list")
        expect(fixture.answered.last?.row.display_name == "Alex Moreno", "the recruiter answered once trails it")
        expect(fixture.fading.prefix(3).map { $0.row.display_name ?? "" }
            == ["The Growth Letter", "SaaS Weekly", "Product Pulse"], "the three unopened newsletters lead the fading list")
        expect(fixture.points.first { $0.row.corrections > 0 }?.sentence.contains("corrected") == true,
            "the vendor's corrections are said")
        expect(Set(MemoryGrouping.groups(memory.entries).map(\.kind)) == Set(MemoryKind.allCases),
            "the memory fixture fills every kind, the unknown one included")
    }

    static func expect(_ cond: Bool, _ what: String) {
        checks += 1
        if !cond {
            failures += 1
            print("  FAIL: \(what)")
        }
    }
}

extension RelationshipRow {
    func withFirst(_ s: String?) -> RelationshipRow {
        var r = self
        r.received_first_at = s
        return r
    }
}
