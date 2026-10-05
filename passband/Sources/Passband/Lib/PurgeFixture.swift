// CANNED SENDERS FOR LOOKING AT THE PURGE AND SMART DONE WITHOUT A MAILBOX.
//
// `PASSBAND_PURGE_FIXTURE=<deck|deck:N|confirm|applying|finish|finish_sites>`
// opens the app straight into that state over the senders below (`deck:N`
// starts N cards in, with those N already decided). `PASSBAND_SMART_DONE_FIXTURE=<working|receipt>` does the
// same for smart done. Same posture as `PASSBAND_FORCE_TESTFLIGHT_NUDGE`: a
// launch environment nobody sets by accident, and never a pref.
//
// WHILE EITHER IS SET, NOTHING HERE REACHES A DAEMON. `AppStore` checks
// `active` at the top of every purge and smart done entry point and simulates
// the answer instead: an unsubscribe is a real request to a real company, and
// a build launched to take a screenshot must not be able to send one.

import Foundation

enum PurgeFixture {
    enum Purge: Equatable {
        case deck(skip: Int)
        case confirm
        case applying
        case finish
        case finishSites
    }

    enum SmartDone: Equatable {
        case working
        case receipt
    }

    static let purge: Purge? = {
        guard let raw = ProcessInfo.processInfo.environment["PASSBAND_PURGE_FIXTURE"] else {
            return nil
        }
        if raw.hasPrefix("deck:"), let n = Int(raw.dropFirst(5)) { return .deck(skip: max(0, n)) }
        switch raw {
        case "deck": return .deck(skip: 0)
        case "confirm": return .confirm
        case "applying": return .applying
        case "finish": return .finish
        case "finish_sites": return .finishSites
        default: return nil
        }
    }()

    static let smartDone: SmartDone? = {
        switch ProcessInfo.processInfo.environment["PASSBAND_SMART_DONE_FIXTURE"] {
        case "working": return .working
        case "receipt": return .receipt
        default: return nil
        }
    }()

    /// True for the whole life of a process launched with either variable.
    static var active: Bool { purge != nil || smartDone != nil }

    static let ledgerDays = 34

    /// What each sender is decided as when a fixture needs a deck already
    /// walked. `siteBound` false trades the site-only unsubscribe for a mute,
    /// for the end card with nothing left to finish.
    static func staged(siteBound: Bool) -> [PurgeDecision] {
        [.unsubscribe, siteBound ? .unsubscribe : .mute, .clear, .clear, .skip, .clear, .mute]
    }

    /// The sender whose decision "fails" at a simulated commit, so the end
    /// card's small line about failures can be looked at.
    static let failing = "hello@ox.example"

    private static func signals(
        received: Int, opened: Int, clearedUnopened: Int = 0, dwellMs: Int? = nil,
        clicks: Int = 0, readFraction: Double? = nil, words: Int? = nil, perMonth: Double
    ) -> PurgeSender.Signals {
        PurgeSender.Signals(
            received: received, opened: opened, cleared_unopened: clearedUnopened,
            dwell_sample: opened, median_dwell_ms: dwellMs, clicks: clicks, read_sample: opened,
            median_read_fraction: readFraction, median_words: words, per_month: perMonth)
    }

    static let senders: [PurgeSender] = [
        // Damning: everything points one way and the daemon can do it itself.
        PurgeSender(
            sender: "deals@dealstream.example", display_name: "Deal Stream Daily",
            open_count: 14, open_message_ids: Array(9001...9014),
            signals: signals(
                received: 23, opened: 1, clearedUnopened: 14, dwellMs: 2_100, perMonth: 22.6),
            unsubscribe: .init(message_id: 9014, auto: true), score: 96,
            reasons: [
                .rarelyOpened(opened: 1, received: 23), .clearedUnopened(count: 14),
                .highVolume(perMonth: 22.6), .neverClicked(opened: 1),
            ]),
        // A long name, and an unsubscribe that ends on the sender's own site.
        PurgeSender(
            sender: "editors@quarterly-review-of-books.example",
            display_name: "The Extremely Long Named Quarterly Review of Books and Letters",
            open_count: 5, open_message_ids: Array(9101...9105),
            signals: signals(
                received: 12, opened: 3, dwellMs: 6_000, readFraction: 0.08, words: 2_400,
                perMonth: 10.4),
            unsubscribe: .init(message_id: 9105, auto: false), score: 82,
            reasons: [
                .skimmed(medianReadFraction: 0.08, medianWords: 2_400, medianDwellMs: 6_000),
                .rarelyOpened(opened: 3, received: 12),
            ]),
        // Already muted: mute is not offered again.
        PurgeSender(
            sender: "perks@parcelperks.example", display_name: "Parcel Perks",
            open_count: 9, open_message_ids: Array(9201...9209),
            signals: signals(
                received: 11, opened: 2, clearedUnopened: 9, dwellMs: 900, perMonth: 9.6),
            rule: .init(id: 7, disposition: .squelch, match_pattern: "perks@parcelperks.example"),
            unsubscribe: .init(message_id: 9209, auto: true), score: 75,
            reasons: [.clearedUnopened(count: 9), .briefLooks(medianDwellMs: 900)]),
        // Read closely: the evidence argues for keeping it.
        PurgeSender(
            sender: "notes@fieldnotes.example", display_name: "Field Notes Weekly",
            open_count: 3, open_message_ids: Array(9301...9303),
            signals: signals(
                received: 9, opened: 8, dwellMs: 240_000, clicks: 5, readFraction: 0.92,
                words: 1_800, perMonth: 4.3),
            unsubscribe: .init(message_id: 9303, auto: true), score: 44,
            reasons: [.readClosely(medianReadFraction: 0.92), .highVolume(perMonth: 4.3)]),
        // Too new to say anything about, and no link to unsubscribe with.
        PurgeSender(
            sender: "team@nimbus.example", display_name: "Nimbus",
            open_count: 1, open_message_ids: [9401],
            signals: signals(received: 1, opened: 0, perMonth: 0.9),
            score: nil, reasons: [.notEnoughHistory]),
        // A two-letter name and a middling score.
        PurgeSender(
            sender: "hello@ox.example", display_name: "Ox",
            open_count: 4, open_message_ids: Array(9501...9504),
            signals: signals(received: 9, opened: 4, dwellMs: 3_000, perMonth: 7.8),
            unsubscribe: .init(message_id: 9504, auto: true), score: 48,
            reasons: [.rarelyOpened(opened: 4, received: 9), .briefLooks(medianDwellMs: 3_000)]),
        // No link, never opened, more mail than the marks can show one by one.
        PurgeSender(
            sender: "rewards@harborbank.example", display_name: "Harbor Bank Rewards",
            open_count: 22, open_message_ids: Array(9601...9622),
            signals: signals(received: 41, opened: 0, clearedUnopened: 12, perMonth: 31.2),
            score: 88,
            reasons: [
                .rarelyOpened(opened: 0, received: 41), .highVolume(perMonth: 31.2),
                .clearedUnopened(count: 12),
            ]),
        // Weak evidence: nothing is suggested.
        PurgeSender(
            sender: "ada@morningswithada.example", display_name: "Mornings with Ada",
            open_count: 2, open_message_ids: [9701, 9702],
            signals: signals(received: 6, opened: 5, dwellMs: 300, perMonth: 5.2),
            unsubscribe: .init(message_id: 9702, auto: true), score: 30,
            reasons: [.briefLooks(medianDwellMs: 300), .neverClicked(opened: 5)]),
    ]

    /// The latest summary lines for each sender, newest first, standing in
    /// for the Reading board the real deck reads them from.
    static let summaries: [String: [String]] = [
        "deals@dealstream.example": [
            "48 hours only: up to 60% off patio furniture, grills and outdoor lighting",
            "Your Tuesday deals are here",
            "Flash sale on headphones ends tonight",
            "Last chance for free shipping",
        ],
        "editors@quarterly-review-of-books.example": [
            "Autumn issue: a very long essay on the letters of three novelists who never met, with a reply from one of their translators and a note on the archive it was found in",
            "From the archive: on rereading",
        ],
        "perks@parcelperks.example": [
            "You have 240 points waiting", "Double points this weekend", "Rate your delivery",
        ],
        "notes@fieldnotes.example": [
            "Issue 212: what the tide tables leave out", "Issue 211: a week of fog",
            "Issue 210: the bridge survey",
        ],
        "team@nimbus.example": ["Welcome to Nimbus"],
        "hello@ox.example": ["New in the shop this week", "Back in stock: the canvas tote"],
        "rewards@harborbank.example": [
            "Earn 5x points on dining this month", "Your rewards summary",
            "A new offer is waiting in your account",
        ],
        "ada@morningswithada.example": ["Thursday: three small things", "Wednesday: on lists"],
    ]

    static let smartDoneResult = SmartDoneResult(
        batch_id: "fixture", cleared: 43, senders: 17,
        by_reason: [
            "event_passed": 3, "offer_ended": 5, "expired": 4, "ignored_sender": 20,
            "stale_promo": 11,
        ], message_ids: [])
}
