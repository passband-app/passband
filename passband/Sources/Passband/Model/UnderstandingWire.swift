// The two wire shapes behind the Usage page's Understanding section
// (docs/UNDERSTANDING.md §2.1 and §2.3): the relationship ledger,
// `GET /client/relationships`, and the owner's memory entries,
// `GET /client/memory`. In a file of their own rather than WireTypes.swift so
// the suites that read them compile with these types alone.
//
// LENIENT EVERYWHERE, more than the rest of the wire. Both shapes are new and
// are being built beside this client, so a field arrives late, a count arrives
// null, a kind arrives that this build never heard of. None of that may cost
// the section: a missing count is zero, a row that will not decode is dropped
// on its own, and an unknown kind is `other` with its line still drawn.

import Foundation

// MARK: - relationships

/// `GET /client/relationships?limit=&sort=`. A daemon older than the ledger
/// answers 404, and the section is hidden.
struct RelationshipsResponse: Codable, Sendable, Equatable {
    /// Days of attention ledger behind the numbers. Under two weeks the
    /// attention half is thin and the page says so.
    var coverage_days: Int
    var computed_at: String?
    var rows: [RelationshipRow]

    init(coverage_days: Int, computed_at: String?, rows: [RelationshipRow]) {
        self.coverage_days = coverage_days
        self.computed_at = computed_at
        self.rows = rows
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        coverage_days = (try? c.decodeIfPresent(Int.self, forKey: .coverage_days)) ?? 0
        computed_at = try? c.decodeIfPresent(String.self, forKey: .computed_at)
        rows = ((try? c.decodeIfPresent([Dropping<RelationshipRow>].self, forKey: .rows)) ?? [])
            .compactMap(\.value)
    }
}

/// One correspondent, as the ledger counts them. Every count the daemon may
/// leave out reads as zero, every rate it may leave out as nil; only the
/// address is required, because a row with no address is nobody.
struct RelationshipRow: Codable, Sendable, Equatable, Identifiable {
    var addr: String
    var display_name: String?
    var received_count: Int
    var received_per_month: Double?
    var received_first_at: String?
    var received_last_at: String?
    var sent_count: Int
    var sent_last_at: String?
    var initiated_count: Int
    var inbound_threads: Int
    var replied_threads: Int
    var median_reply_ms: Int?
    var attention_rows: Int
    var opened_count: Int
    var median_read_fraction: Double?
    var link_clicks: Int
    var corrections: Int
    var fatigue_score: Double?
    var fatigue_reasons: [FatigueReason]

    var id: String { addr }

    init(
        addr: String, display_name: String? = nil, received_count: Int = 0,
        received_per_month: Double? = nil, received_first_at: String? = nil,
        received_last_at: String? = nil, sent_count: Int = 0, sent_last_at: String? = nil,
        initiated_count: Int = 0, inbound_threads: Int = 0, replied_threads: Int = 0,
        median_reply_ms: Int? = nil, attention_rows: Int = 0, opened_count: Int = 0,
        median_read_fraction: Double? = nil, link_clicks: Int = 0, corrections: Int = 0,
        fatigue_score: Double? = nil, fatigue_reasons: [FatigueReason] = []
    ) {
        self.addr = addr
        self.display_name = display_name
        self.received_count = received_count
        self.received_per_month = received_per_month
        self.received_first_at = received_first_at
        self.received_last_at = received_last_at
        self.sent_count = sent_count
        self.sent_last_at = sent_last_at
        self.initiated_count = initiated_count
        self.inbound_threads = inbound_threads
        self.replied_threads = replied_threads
        self.median_reply_ms = median_reply_ms
        self.attention_rows = attention_rows
        self.opened_count = opened_count
        self.median_read_fraction = median_read_fraction
        self.link_clicks = link_clicks
        self.corrections = corrections
        self.fatigue_score = fatigue_score
        self.fatigue_reasons = fatigue_reasons
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        addr = try c.decode(String.self, forKey: .addr)
        func int(_ k: CodingKeys) -> Int { (try? c.decodeIfPresent(Int.self, forKey: k)) ?? 0 }
        func num(_ k: CodingKeys) -> Double? { (try? c.decodeIfPresent(Double.self, forKey: k)) ?? nil }
        func str(_ k: CodingKeys) -> String? { (try? c.decodeIfPresent(String.self, forKey: k)) ?? nil }
        display_name = str(.display_name).flatMap { $0.isEmpty ? nil : $0 }
        received_count = int(.received_count)
        received_per_month = num(.received_per_month)
        received_first_at = str(.received_first_at)
        received_last_at = str(.received_last_at)
        sent_count = int(.sent_count)
        sent_last_at = str(.sent_last_at)
        initiated_count = int(.initiated_count)
        inbound_threads = int(.inbound_threads)
        replied_threads = int(.replied_threads)
        median_reply_ms = (try? c.decodeIfPresent(Int.self, forKey: .median_reply_ms)) ?? nil
        attention_rows = int(.attention_rows)
        opened_count = int(.opened_count)
        median_read_fraction = num(.median_read_fraction)
        link_clicks = int(.link_clicks)
        corrections = int(.corrections)
        fatigue_score = num(.fatigue_score)
        fatigue_reasons = ((try? c.decodeIfPresent([Dropping<FatigueReason>].self, forKey: .fatigue_reasons)) ?? [])
            .compactMap(\.value)
    }
}

/// One of `sender_fatigue`'s reasons, as the daemon serializes its `Reason`
/// enum: `#[serde(tag = "code", rename_all = "snake_case")]`, so the code and
/// the variant's own fields side by side. The variants this build knows:
///
///   * `rarely_opened { opened, received }`
///   * `cleared_unopened { count }`
///   * `never_clicked { opened }`
///   * `skimmed { median_read_fraction, median_words, median_dwell_ms }`
///   * `brief_looks { median_dwell_ms }`, `read_closely { median_read_fraction }`
///   * `high_volume { per_month }`
///   * `not_enough_history`, `known_contact`, `wanted_by_rule`, `kept_recently`
///
/// Every numeric field is kept, by name, as a Double (several are f32 on the
/// daemon); anything else is ignored, and a code this build does not know is
/// kept and simply never quoted.
struct FatigueReason: Codable, Sendable, Equatable {
    var code: String
    var values: [String: Double]

    init(code: String, values: [String: Double] = [:]) {
        self.code = code
        self.values = values
    }

    /// A field as a whole number, for a count.
    func int(_ key: String) -> Int? { values[key].map { Int($0.rounded()) } }

    private struct Key: CodingKey {
        var stringValue: String
        var intValue: Int? { nil }
        init(stringValue: String) { self.stringValue = stringValue }
        init?(intValue: Int) { nil }
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: Key.self)
        code = try c.decode(String.self, forKey: Key(stringValue: "code"))
        var values: [String: Double] = [:]
        for key in c.allKeys where key.stringValue != "code" {
            if let n = try? c.decode(Double.self, forKey: key) { values[key.stringValue] = n }
        }
        self.values = values
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: Key.self)
        try c.encode(code, forKey: Key(stringValue: "code"))
        for (k, v) in values { try c.encode(v, forKey: Key(stringValue: k)) }
    }
}

// MARK: - memory

/// `GET /client/memory`. Behind the `understanding` hidden feature: with it
/// off the route 404s and the list is not drawn.
struct MemoryResponse: Codable, Sendable, Equatable {
    var enabled: Bool
    var coverage_days: Int
    var entries: [MemoryEntry]

    init(enabled: Bool, coverage_days: Int, entries: [MemoryEntry]) {
        self.enabled = enabled
        self.coverage_days = coverage_days
        self.entries = entries
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        enabled = (try? c.decodeIfPresent(Bool.self, forKey: .enabled)) ?? false
        coverage_days = (try? c.decodeIfPresent(Int.self, forKey: .coverage_days)) ?? 0
        entries = ((try? c.decodeIfPresent([Dropping<MemoryEntry>].self, forKey: .entries)) ?? [])
            .compactMap(\.value)
    }
}

/// Who wrote an entry. `stated` is the owner's own; `inferred` is the
/// nightly pass's guess. An origin this build does not know reads as
/// inferred: the weaker claim, so nothing is ever shown as the owner's own
/// words by accident.
enum MemoryOrigin: String, LenientRawEnum {
    case stated, inferred
    static var unknownFallback: MemoryOrigin { .inferred }
}

/// The typed kinds the pass may write. A kind added on the daemon after this
/// build is `other`, and its line is still drawn.
enum MemoryKind: String, LenientRawEnum, CaseIterable {
    case role, project, person, prefers, ignores, other
    static var unknownFallback: MemoryKind { .other }
}

struct MemoryProvenance: Codable, Sendable, Equatable {
    var message_ids: [Int]
    var addrs: [String]

    init(message_ids: [Int] = [], addrs: [String] = []) {
        self.message_ids = message_ids
        self.addrs = addrs
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        message_ids = (try? c.decodeIfPresent([Int].self, forKey: .message_ids)) ?? []
        addrs = (try? c.decodeIfPresent([String].self, forKey: .addrs)) ?? []
    }
}

struct MemoryEntry: Codable, Sendable, Equatable, Identifiable {
    var id: String
    /// `owner`, `sender:<addr>`, `domain:<d>` or `kind:<slug>`.
    var scope: String
    var origin: MemoryOrigin
    var kind: MemoryKind
    /// The typed fields, kept only where they are strings: every field the
    /// design gives a kind is one (`what`, `name`, `note`, `until`, `addr`,
    /// `relation`, `basis`). The client draws `line`, not these.
    var body: [String: String]
    var line: String
    var provenance: MemoryProvenance
    var confidence: Double
    var created_at: String?
    var updated_at: String?
    var affirmed_at: String?
    var expires_at: String?
    var retired_at: String?

    init(
        id: String, scope: String = "owner", origin: MemoryOrigin, kind: MemoryKind,
        body: [String: String] = [:], line: String, provenance: MemoryProvenance = MemoryProvenance(),
        confidence: Double = 0.5, created_at: String? = nil, updated_at: String? = nil,
        affirmed_at: String? = nil, expires_at: String? = nil, retired_at: String? = nil
    ) {
        self.id = id
        self.scope = scope
        self.origin = origin
        self.kind = kind
        self.body = body
        self.line = line
        self.provenance = provenance
        self.confidence = confidence
        self.created_at = created_at
        self.updated_at = updated_at
        self.affirmed_at = affirmed_at
        self.expires_at = expires_at
        self.retired_at = retired_at
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        line = try c.decode(String.self, forKey: .line)
        scope = (try? c.decodeIfPresent(String.self, forKey: .scope)) ?? "owner"
        origin = (try? c.decodeIfPresent(MemoryOrigin.self, forKey: .origin)) ?? .inferred
        kind = (try? c.decodeIfPresent(MemoryKind.self, forKey: .kind)) ?? .other
        let raw = (try? c.decodeIfPresent([String: Dropping<String>].self, forKey: .body)) ?? [:]
        body = raw.compactMapValues(\.value)
        provenance = (try? c.decodeIfPresent(MemoryProvenance.self, forKey: .provenance)) ?? MemoryProvenance()
        confidence = (try? c.decodeIfPresent(Double.self, forKey: .confidence)) ?? 0
        func str(_ k: CodingKeys) -> String? { (try? c.decodeIfPresent(String.self, forKey: k)) ?? nil }
        created_at = str(.created_at)
        updated_at = str(.updated_at)
        affirmed_at = str(.affirmed_at)
        expires_at = str(.expires_at)
        retired_at = str(.retired_at)
    }

    var isRetired: Bool { retired_at != nil }

    /// Why an entry no longer holds, or nil while it does. The daemon serves
    /// all three: an entry the pass retired, an inferred entry past its
    /// `expires_at` that no pass re-affirmed, and a project whose `until`
    /// has gone by. Each is drawn crossed out, and none can be confirmed.
    enum Withdrawal: Equatable, Sendable { case retired, expired, ended }

    func withdrawn(now: Date) -> Withdrawal? {
        if retired_at != nil { return .retired }
        if let at = MemoryDates.instant(expires_at), at <= now { return .expired }
        if kind == .project, let until = MemoryDates.endOfDay(body["until"]), until <= now { return .ended }
        return nil
    }
}

/// The few dates the memory wire carries, parsed here so the entry can say
/// whether it holds without the section's formatting around it.
enum MemoryDates {
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

    static func instant(_ s: String?) -> Date? {
        guard let s, !s.isEmpty else { return nil }
        return iso.date(from: s) ?? isoFractional.date(from: s)
    }

    /// A project's `until`: an instant, or a "YYYY-MM-DD" day, which lasts to
    /// the end of that day in the reader's zone.
    static func endOfDay(_ s: String?, calendar: Calendar = .current) -> Date? {
        guard let s, !s.isEmpty else { return nil }
        if let at = instant(s) { return at }
        let parts = s.split(separator: "-").compactMap { Int($0) }
        guard parts.count == 3,
            let day = calendar.date(from: DateComponents(year: parts[0], month: parts[1], day: parts[2]))
        else { return nil }
        return calendar.date(byAdding: .day, value: 1, to: day)
    }
}

/// `PUT /client/memory/{id}`. Every field optional and left off the wire
/// when nil: `confirm` alone flips an inferred entry to stated.
struct MemoryPatch: Encodable, Sendable {
    var line: String?
    var confirm: Bool?
}

// MARK: - the flag

/// The `understanding` hidden feature's key, the same string as
/// `squelch-core/src/features.rs`. Here rather than only in HiddenFeatures.swift
/// so the practice transport, which compiles without the app, can claim it.
enum UnderstandingFlag {
    static let key = "understanding"
}

// MARK: - helper

/// An element that decodes as nil instead of failing its array, so one bad
/// row costs that row and not the list around it.
struct Dropping<T: Decodable>: Decodable {
    let value: T?
    init(from decoder: Decoder) throws {
        value = try? T(from: decoder)
    }
}
