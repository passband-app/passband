// WHAT SOMEBODY HAS FOUND IN THE APP, and whether it stuck. The tracking half
// of discovery; the ring and the menu that read it live elsewhere.
//
// Eight feature sets, each a handful of moves. A set is unseen, then FOUND the
// first time any of its moves happens on real mail, then KEPT once its moves
// have happened on `keptAfterDays` distinct local days. Kept is the honest
// version of "uses it": one curious afternoon is a find, three separate days
// is a habit.
//
// NOTHING REGRESSES. There are no streaks and no decay: a set kept in March is
// still kept after a month away. A progress ring that drains is a guilt meter,
// and the app has no business making anybody feel behind on their email.
//
// PURE, Foundation only, every input passed in (the day, the clock, the mode),
// so a headless suite pins it. The UserDefaults shell is `Discovery`.

import Foundation

/// The ring, in ring order. The raw values are persisted AND leave the machine
/// as `feature_found`/`feature_kept` properties, so a rename is a migration and
/// an Analytics vocabulary change, not a refactor.
enum FeatureSet: String, CaseIterable, Sendable, Codable {
    case clear, flow, later, teach, find, read, glance, write
}

/// One thing a person did that counts toward a set. Raw values are persisted
/// (as dictionary keys), so the same caution applies.
enum Move: String, CaseIterable, Sendable, Codable {
    // clear: the basics the tour teaches
    case tourCompleted, emailDone
    // flow: the Space walk through For Your Eyes
    case walkDeep, walkFinished
    // later
    case reminderSet
    // teach
    case ruleSaved, triageCorrected, unsubscribed
    // find
    case searchHitOpened, assistantAsked
    // read
    case stackCleared
    // glance
    case shipmentCleared, codeCopied
    // write
    case sendScheduled, groupSent

    /// Exhaustive on purpose: a new move that forgets its set does not compile.
    var set: FeatureSet {
        switch self {
        case .tourCompleted, .emailDone: .clear
        case .walkDeep, .walkFinished: .flow
        case .reminderSet: .later
        case .ruleSaved, .triageCorrected, .unsubscribed: .teach
        case .searchHitOpened, .assistantAsked: .find
        case .stackCleared: .read
        case .shipmentCleared, .codeCopied: .glance
        case .sendScheduled, .groupSent: .write
        }
    }
}

enum SetState: Int, Comparable, Sendable {
    case unseen, found, kept

    static func < (a: SetState, b: SetState) -> Bool { a.rawValue < b.rawValue }
}

/// What one `note` changed. At most one per call, so the shell never has to
/// choose between celebrating a find and announcing a keep.
enum DiscoveryEvent: Equatable, Sendable {
    case found(FeatureSet)
    case kept(FeatureSet)
}

struct DiscoveryLedger: Equatable, Sendable {
    /// Distinct local days of use before a set counts as kept.
    static let keptAfterDays = 3

    /// Bumped only when an OLD build could no longer make sense of the blob.
    /// Adding a set or a move is not that: unknown keys are skipped on decode.
    static let version = 1

    struct SetRecord: Codable, Equatable, Sendable {
        var foundAt: Date?
        var keptAt: Date?
        /// Distinct day keys on which any of the set's moves happened. Capped
        /// at `keptAfterDays`: past that the answer cannot change, and a list
        /// that grows forever in UserDefaults is a slow leak.
        var days: [String] = []
    }

    private(set) var sets: [FeatureSet: SetRecord] = [:]
    /// Every time a move happened, kept for contextual hints later ("you have
    /// set 40 reminders"). Nothing reads it yet.
    private(set) var moveCounts: [Move: Int] = [:]
    /// Found and not yet looked at in the menu. Drives the ring's "new" glow.
    private(set) var fresh: Set<FeatureSet> = []
    /// The day key of the last celebration, so a busy first day is one
    /// moment, not eight.
    var lastCelebratedDay: String?

    init() {}

    func state(of set: FeatureSet) -> SetState {
        guard let record = sets[set] else { return .unseen }
        if record.keptAt != nil { return .kept }
        return record.foundAt != nil ? .found : .unseen
    }

    var allKept: Bool { FeatureSet.allCases.allSatisfy { state(of: $0) == .kept } }

    /// Record one move. Returns what CHANGED, if anything: at most one event.
    ///
    /// The day set is per SET, the union of all its moves: a reminder on Monday
    /// and an unrelated reminder on Thursday are two days of `later`, and so is
    /// a rule on Monday and a correction on Thursday for `teach`. The finding
    /// day is the first of the three, which is why a set can never be found
    /// and kept in the same call.
    mutating func note(_ move: Move, day: String, now: Date) -> DiscoveryEvent? {
        moveCounts[move, default: 0] += 1
        let set = move.set
        var record = sets[set] ?? SetRecord()
        defer { sets[set] = record }

        if record.foundAt == nil {
            record.foundAt = now
            record.days = [day]
            fresh.insert(set)
            return .found(set)
        }
        guard record.keptAt == nil, !record.days.contains(day) else { return nil }
        record.days.append(day)
        guard record.days.count >= Self.keptAfterDays else { return nil }
        record.keptAt = now
        return .kept(set)
    }

    /// Record a move that happened BEFORE this ledger existed: an existing user
    /// who finished the tour months ago did not just discover anything. The
    /// move lands exactly as `note` would land it, minus the freshness; the
    /// caller gets no event because there is nothing to announce.
    mutating func backfill(_ move: Move, day: String, now: Date) {
        let wasFresh = fresh.contains(move.set)
        _ = note(move, day: day, now: now)
        if !wasFresh { fresh.remove(move.set) }
    }

    /// The menu was opened: nothing is fresh any more.
    mutating func acknowledge() { fresh.removeAll() }

    /// Practice mail never counts: an `e` on a fixture email teaches the hand,
    /// it is not somebody finding done. The one exception is the tour's own
    /// completion, which can only ever happen in practice.
    static func counts(_ move: Move, practice: Bool) -> Bool {
        !practice || move == .tourCompleted
    }

    /// At most one celebration per local day. Nil means never celebrated.
    static func mayCelebrate(lastCelebratedDay: String?, today: String) -> Bool {
        lastCelebratedDay != today
    }

    /// A local day as a stable string, the same shape ShareNudge uses. Local
    /// rather than UTC: "three different days" means the person's days, and a
    /// morning and an evening are not two days because UTC rolled over between.
    static func dayKey(_ date: Date, calendar: Calendar = .current) -> String {
        let c = calendar.dateComponents([.year, .month, .day], from: date)
        return "\(c.year ?? 0)-\(c.month ?? 0)-\(c.day ?? 0)"
    }

    // MARK: - debug override

    /// A ledger in exactly these states, for `PASSBAND_DISCOVERY`. A forced
    /// keep gets synthetic day keys so it reads as kept by every path, not
    /// just `state(of:)`. Nothing is fresh unless named in `fresh` (the
    /// override's `:fresh` entries), and only a set that is found or kept can
    /// be: the glow means "found and not yet looked at", never "unseen".
    static func forcing(
        _ states: [FeatureSet: SetState], fresh: Set<FeatureSet> = [], now: Date
    ) -> DiscoveryLedger {
        var ledger = DiscoveryLedger()
        ledger.fresh = fresh.filter { (states[$0] ?? .unseen) != .unseen }
        for (set, state) in states where state != .unseen {
            var record = SetRecord(foundAt: now)
            if state == .kept {
                record.keptAt = now
                record.days = (1...keptAfterDays).map { "forced-\($0)" }
            } else {
                record.days = ["forced-1"]
            }
            ledger.sets[set] = record
        }
        return ledger
    }

    /// `"clear:kept,flow:found"` to states. Entries that do not parse are
    /// skipped rather than failing the lot: a typo in a debug variable should
    /// cost that one set, not the whole look. `fresh` is FOUND as far as the
    /// state goes; `parseFreshOverride` is what reads it as the glow.
    static func parseOverride(_ spec: String) -> [FeatureSet: SetState] {
        var states: [FeatureSet: SetState] = [:]
        for (set, word) in overrideEntries(spec) {
            switch word {
            case "unseen": states[set] = .unseen
            case "found", "fresh": states[set] = .found
            case "kept": states[set] = .kept
            default: continue
            }
        }
        return states
    }

    /// The sets an override spelled `:fresh`: found, and glowing on the ring
    /// as if the menu had not been opened since.
    static func parseFreshOverride(_ spec: String) -> Set<FeatureSet> {
        Set(overrideEntries(spec).filter { $0.word == "fresh" }.map(\.set))
    }

    private static func overrideEntries(_ spec: String) -> [(set: FeatureSet, word: String)] {
        spec.split(separator: ",").compactMap { entry in
            let parts = entry.split(separator: ":").map {
                $0.trimmingCharacters(in: .whitespaces).lowercased()
            }
            guard parts.count == 2, let set = FeatureSet(rawValue: parts[0]) else { return nil }
            return (set, parts[1])
        }
    }
}

// MARK: - persistence

/// KEYED BY RAW STRING, AND TOLERANT BOTH WAYS. Synthesized Codable on an
/// enum-keyed dictionary throws on the first key it does not recognise, which
/// means a downgrade (or a later rename) would wipe somebody's whole ledger on
/// the next save. So the dictionaries go out as `[String: …]`, and on the way
/// in a key this build does not know is skipped, not fatal.
extension DiscoveryLedger: Codable {
    private enum CodingKeys: String, CodingKey {
        case version, sets, moves, fresh, lastCelebratedDay
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        // Each section on its own `try?`: a malformed `moves` from some future
        // build must not take the sets, which are the part anybody can see.
        let rawSets = (try? c.decodeIfPresent([String: SetRecord].self, forKey: .sets)) ?? nil
        for (key, record) in rawSets ?? [:] {
            guard let set = FeatureSet(rawValue: key) else { continue }
            var record = record
            record.days = Array(record.days.prefix(Self.keptAfterDays))
            sets[set] = record
        }
        let rawMoves = (try? c.decodeIfPresent([String: Int].self, forKey: .moves)) ?? nil
        for (key, count) in rawMoves ?? [:] {
            guard let move = Move(rawValue: key) else { continue }
            moveCounts[move] = count
        }
        let rawFresh = (try? c.decodeIfPresent([String].self, forKey: .fresh)) ?? nil
        fresh = Set((rawFresh ?? []).compactMap(FeatureSet.init(rawValue:)))
        lastCelebratedDay = (try? c.decodeIfPresent(String.self, forKey: .lastCelebratedDay)) ?? nil
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(Self.version, forKey: .version)
        try c.encode(
            Dictionary(uniqueKeysWithValues: sets.map { ($0.key.rawValue, $0.value) }),
            forKey: .sets)
        try c.encode(
            Dictionary(uniqueKeysWithValues: moveCounts.map { ($0.key.rawValue, $0.value) }),
            forKey: .moves)
        try c.encode(fresh.map(\.rawValue).sorted(), forKey: .fresh)
        try c.encodeIfPresent(lastCelebratedDay, forKey: .lastCelebratedDay)
    }

    /// The blob as stored, or an empty ledger. Never throws: a ledger that
    /// cannot be read is a ledger starting over, not an app that will not open.
    static func decoded(from data: Data?) -> DiscoveryLedger {
        guard let data, let ledger = try? JSONDecoder().decode(Self.self, from: data)
        else { return DiscoveryLedger() }
        return ledger
    }

    func encoded() -> Data? { try? JSONEncoder().encode(self) }
}
