// QUICK CLEAR on the Mac: the daemon's "you have a few minutes, here is what
// fits in them" banner, sent when the owner is likely at their desk. This file
// is the pure half, so the test suite compiles it alone: the frame, whether it
// is fresh, what the banner says, and the userInfo a tap reads back.
// `Notifier.postQuickClear` posts it; `EventStream` hands it over; the sprint
// it opens is Model/QuickClearSprint.swift and Views/QuickClearView.swift.
//
// Built exactly like smart re-notify (RenotifyBanner.swift, docs/RENOTIFY.md):
// the frame rides the Mac stream with no `id:` and no `Event` shape, so it
// moves no cursor and is decoded only after a frame failed to decode as an
// event; a late one is dropped by the `open_draft` freshness rule; its identity
// is the BATCH, account-prefixed, never an event's, so `ToldEvents` never
// swallows it. The tap half (keys and `quickClearTap`) lives in
// EventBanner.swift, because `tapRoute` reads it and the phone's notification
// extension compiles that file without this one.
//
// THE PROMISE IS THE QUEUE. "5 minutes clears 14 of your 23 emails" is only
// honest if the tap opens exactly those 14, so the frame carries them, and the
// banner's userInfo carries the frame (see `quickClearUserInfo`): a tap after
// a relaunch still opens the queue the banner promised.

import Foundation

/// The `quick_clear` frame:
/// `{"quick_clear": <batch>, "trigger": "...", "at": <unix secs>,
///   "budget_secs": 300, "est_secs": 284, "fit_count": 14, "open_count": 23,
///   "smart_clear_count": 6, "items": [...]}`.
///
/// LENIENT on purpose, past what `RenotifySignal` is: every field but the
/// batch is optional, an item that does not decode is dropped rather than
/// failing the frame, and an unknown `basis` reads as `.unknown`. A daemon one
/// version ahead must still get its banner shown.
struct QuickClearSignal: Codable, Equatable, Sendable {
    struct Item: Codable, Equatable, Sendable {
        var message_id: Int
        var thread_id: String
        /// Not sent by the daemon and never read: the stream the frame
        /// arrived on IS the account, as for every other frame. Decoded only
        /// so a daemon that does send one is not refused.
        var account: String?
        /// The daemon's guess at how long this one takes, in seconds.
        var est_secs: Int?
        /// What the guess rests on. Display never depends on it.
        var basis: Basis?
        /// The daemon's word for what the mail is: `smart_clear`, `glance`,
        /// `receipt`, `delivery`, `account_service`, `review`, `attend`,
        /// `read`, or a newer one. Kept as words, shown as a small tag, never
        /// sent to analytics. `smart_clear` rows are the ones the sweep step
        /// clears; they come first, with `est_secs` 0.
        var kind: String?

        /// One of the rows the smart clear step covers.
        var isSmartClear: Bool { kind == Self.smartClearKind }
        static let smartClearKind = "smart_clear"
        var sender: String
        var one_line: String

        init(
            message_id: Int, thread_id: String, account: String? = nil, est_secs: Int? = nil,
            basis: Basis? = nil, kind: String? = nil, sender: String, one_line: String
        ) {
            self.message_id = message_id
            self.thread_id = thread_id
            self.account = account
            self.est_secs = est_secs
            self.basis = basis
            self.kind = kind
            self.sender = sender
            self.one_line = one_line
        }

        init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            message_id = try c.decode(Int.self, forKey: .message_id)
            thread_id = (try? c.decodeIfPresent(String.self, forKey: .thread_id)) ?? ""
            account = try? c.decodeIfPresent(String.self, forKey: .account)
            est_secs = try? c.decodeIfPresent(Int.self, forKey: .est_secs)
            basis = try? c.decodeIfPresent(Basis.self, forKey: .basis)
            kind = try? c.decodeIfPresent(String.self, forKey: .kind)
            sender = (try? c.decodeIfPresent(String.self, forKey: .sender)) ?? ""
            one_line = (try? c.decodeIfPresent(String.self, forKey: .one_line)) ?? ""
        }
    }

    /// Where an item's time estimate came from. Lenient: a newer daemon's
    /// basis reads as `.unknown` rather than dropping the item.
    enum Basis: String, LenientRawEnum {
        case smart_clear, dwell, model, heuristic, unknown
        static var unknownFallback: Self { .unknown }
    }

    let quick_clear: Int
    var trigger: String?
    var at: Int?
    var budget_secs: Int?
    var est_secs: Int?
    var fit_count: Int?
    var open_count: Int?
    var smart_clear_count: Int?
    var items: [Item]

    init(
        quick_clear: Int, trigger: String? = nil, at: Int? = nil, budget_secs: Int? = nil,
        est_secs: Int? = nil, fit_count: Int? = nil, open_count: Int? = nil,
        smart_clear_count: Int? = nil, items: [Item]
    ) {
        self.quick_clear = quick_clear
        self.trigger = trigger
        self.at = at
        self.budget_secs = budget_secs
        self.est_secs = est_secs
        self.fit_count = fit_count
        self.open_count = open_count
        self.smart_clear_count = smart_clear_count
        self.items = items
    }

    /// One item that may not decode, so one bad row costs that row only.
    private struct LossyItem: Decodable {
        let item: Item?
        init(from decoder: Decoder) throws { item = try? Item(from: decoder) }
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        quick_clear = try c.decode(Int.self, forKey: .quick_clear)
        trigger = try? c.decodeIfPresent(String.self, forKey: .trigger)
        at = try? c.decodeIfPresent(Int.self, forKey: .at)
        budget_secs = try? c.decodeIfPresent(Int.self, forKey: .budget_secs)
        est_secs = try? c.decodeIfPresent(Int.self, forKey: .est_secs)
        fit_count = try? c.decodeIfPresent(Int.self, forKey: .fit_count)
        open_count = try? c.decodeIfPresent(Int.self, forKey: .open_count)
        smart_clear_count = try? c.decodeIfPresent(Int.self, forKey: .smart_clear_count)
        let raw = (try? c.decodeIfPresent([LossyItem].self, forKey: .items)) ?? []
        // Queue order is the daemon's; a repeated message keeps its first slot.
        var seen = Set<Int>()
        items = raw.compactMap(\.item).filter { $0.message_id > 0 && seen.insert($0.message_id).inserted }
    }

    // MARK: - derived

    /// The time the promise is made against. Five minutes when the daemon did
    /// not say, and never under a minute or over an hour whatever it said.
    var budget: Int { min(3600, max(60, budget_secs ?? 300)) }
    /// How many the budget clears: the daemon's count, else the queue's.
    var fit: Int { max(0, fit_count ?? items.count) }
    /// How many are waiting in all. Never fewer than fit: a frame that says
    /// otherwise reads as "everything fits".
    var open: Int { max(fit, open_count ?? fit) }
    /// How many the sweep step clears: the daemon's count, and never fewer
    /// than the `smart_clear` rows the queue itself carries.
    var smartClears: Int { max(0, smart_clear_count ?? 0, items.filter(\.isSmartClear).count) }
    /// The queue's own time, in seconds: the daemon's total, else the items'.
    var estimate: Int {
        if let est_secs, est_secs > 0 { return est_secs }
        let sum = items.reduce(0) { $0 + max(0, $1.est_secs ?? 0) }
        return sum > 0 ? sum : budget
    }

    /// The trigger, when it is one this app knows (the daemon's re-notify
    /// set). Only this reaches analytics.
    var knownTrigger: EventBanner.RenotifyTrigger? {
        trigger.flatMap(EventBanner.RenotifyTrigger.init(rawValue:))
    }

    static func parse(_ data: String) -> QuickClearSignal? {
        guard let bytes = data.data(using: .utf8),
            let signal = try? JSONDecoder().decode(QuickClearSignal.self, from: bytes),
            signal.quick_clear > 0,
            // Nothing to clear is no banner: no queue, and no smart clear step.
            !signal.items.isEmpty || signal.smartClears > 0
        else { return nil }
        return signal
    }

    /// THE SAME RULE AS `open_draft` and `renotify` (`AgentOpenSignal.freshFor`):
    /// "you have five minutes now" read an hour later, when the lid opens, is
    /// a claim about a moment that has passed. A frame with no date is fresh.
    func isFresh(now: Int) -> Bool {
        guard let at else { return true }
        return abs(now - at) <= AgentOpenSignal.freshFor
    }

    /// The frame as the banner's userInfo carries it: JSON, with each line
    /// capped, so a tap after a relaunch still has the queue it promised.
    /// The words are the same ones the banner and the board already show.
    var payload: String {
        var copy = self
        copy.items = items.prefix(Self.payloadItems).map {
            var item = $0
            item.sender = EventBanner.flatten(item.sender, max: 120)
            item.one_line = EventBanner.flatten(item.one_line, max: 200)
            item.account = nil
            return item
        }
        guard let data = try? JSONEncoder().encode(copy) else { return "" }
        return String(decoding: data, as: UTF8.self)
    }

    /// Far past any queue five minutes holds; a guard on the userInfo size,
    /// not a product decision.
    static let payloadItems = 60
}

extension EventBanner {
    /// The request identifier, NAMESPACED BY ACCOUNT like re-notify's: batch
    /// ids are one daemon's SQLite ints.
    static func quickClearIdentifier(account: UUID, batch: Int) -> String {
        "passband.quickclear.\(account.uuidString).\(batch)"
    }

    /// The userInfo a quick clear banner carries. No `eventKey` and no
    /// `messageKey`: the tap opens a queue, never one message, and nothing
    /// here may fall through to the reader.
    static func quickClearUserInfo(_ signal: QuickClearSignal, account: UUID) -> [AnyHashable: Any] {
        var info: [AnyHashable: Any] = [
            routeKey: quickClearRoute,
            quickClearBatchKey: signal.quick_clear,
            quickClearCountKey: signal.items.count,
            quickClearPayloadKey: signal.payload,
            accountKey: account.uuidString,
        ]
        if let trigger = signal.trigger { info[quickClearTriggerKey] = trigger }
        return info
    }

    // MARK: - copy

    /// What a quick clear banner says. The owner's rules:
    ///
    /// - LEAD WITH COUNTS: "5 minutes clears 14 of your 23 emails".
    /// - THE PERCENT ONLY WHEN IT FLATTERS AND IS TRUE: at least half. "That
    ///   is 26% of what is waiting" is a reason not to tap.
    /// - A SMALL BACKLOG (everything fits) is said as what it is: "6 quick
    ///   ones, about 3 minutes".
    /// - No dashes anywhere, and no chime: this is an offer, not an alarm.
    static func quickClearCopy(for signal: QuickClearSignal) -> Copy {
        Copy(
            title: quickClearTitle(signal), body: quickClearBody(signal),
            threadIdentifier: quickClearGroup, sound: false)
    }

    static func quickClearTitle(_ signal: QuickClearSignal) -> String {
        let fit = signal.fit
        let open = signal.open
        if fit == 0 {
            // Only the sweep: no queue to promise a time against.
            return "One press clears \(smartClearPhrase(signal.smartClears))"
        }
        if fit >= open {
            let what = fit == 1 ? "1 quick one" : "\(fit) quick ones"
            return "\(what), \(aboutMinutes(signal.estimate))"
        }
        return "\(minutes(signal.budget)) clears \(fit) of your \(open) emails"
    }

    static func quickClearBody(_ signal: QuickClearSignal) -> String {
        var lines: [String] = []
        if let pct = quickClearPercent(fit: signal.fit, open: signal.open) {
            lines.append("That is \(pct)% of what is waiting.")
        }
        if signal.smartClears > 0, signal.fit > 0 {
            lines.append("First, one press clears \(smartClearPhrase(signal.smartClears)).")
        }
        if lines.isEmpty {
            return signal.fit == 0 ? "You can undo it." : "One at a time, at your pace."
        }
        return lines.joined(separator: " ")
    }

    /// The share of the backlog the queue clears, as a whole percent, or nil
    /// when it should not be said: under half (it would argue against the
    /// tap), or when everything fits (the small copy says that already).
    static func quickClearPercent(fit: Int, open: Int) -> Int? {
        guard open > 0, fit > 0, fit < open else { return nil }
        let pct = Int((Double(fit) * 100 / Double(open)).rounded())
        return pct >= 50 ? min(pct, 99) : nil
    }

    /// "6 expired promos and past events", "1 expired promo or past event".
    /// The smart clear step's words, shared by the banner and the sprint.
    static func smartClearPhrase(_ count: Int) -> String {
        count == 1 ? "1 expired promo or past event" : "\(count) expired promos and past events"
    }

    /// "5 minutes", "1 minute": a budget, rounded to the nearest minute.
    static func minutes(_ secs: Int) -> String {
        let m = max(1, Int((Double(secs) / 60).rounded()))
        return m == 1 ? "1 minute" : "\(m) minutes"
    }

    /// "about 3 minutes", "about 1 minute", or "under a minute": an estimate.
    static func aboutMinutes(_ secs: Int) -> String {
        if secs < 60 { return "under a minute" }
        return "about \(minutes(secs))"
    }
}
