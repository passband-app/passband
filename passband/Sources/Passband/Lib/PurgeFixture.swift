// CANNED SENDERS FOR LOOKING AT THE PURGE AND SMART CLEAR WITHOUT A MAILBOX.
//
// `PASSBAND_PURGE_FIXTURE=<pill|few|parked|expired|deck|deck:N|confirm|
// applying|finish|finish_running|finish_needs>` opens the app straight into that state over
// the senders below. `pill` only makes the purge pill appear (enough
// candidates), `few` leaves too few for it to; `deck:N` starts N senders in
// with those N already decided. `PASSBAND_PURGE_RULES=<open|loading|none|
// other>` brings the sender page up with "Create rule" already pressed;
// `PASSBAND_PURGE_BODY=<loading|failed>` and `PASSBAND_PURGE_SELECT=N` set
// the rendered email's state and which one in the strip is selected.
// `PASSBAND_SMART_CLEAR_FIXTURE=<working|receipt|list>` does the same for
// smart clear. Same posture as `PASSBAND_FORCE_TESTFLIGHT_NUDGE`: a launch
// environment nobody sets by accident, and never a pref.
//
// WHILE EITHER IS SET, NOTHING HERE REACHES A DAEMON. `AppStore` checks
// `active` at the top of every purge and smart clear entry point and simulates
// the answer instead: an unsubscribe is a real request to a real company, and
// a build launched to take a screenshot must not be able to send one.

import Foundation

enum PurgeFixture {
    enum Purge: Equatable {
        case pill
        case few
        case parked
        case expired
        case deck(skip: Int)
        case confirm
        case applying
        case finish
        case finishRunning
        case finishNeeds
    }

    enum SmartClear: Equatable {
        case working
        case receipt
        case list
    }

    static let purge: Purge? = {
        guard let raw = ProcessInfo.processInfo.environment["PASSBAND_PURGE_FIXTURE"] else {
            return nil
        }
        if raw.hasPrefix("deck:"), let n = Int(raw.dropFirst(5)) { return .deck(skip: max(0, n)) }
        switch raw {
        case "pill": return .pill
        case "few": return .few
        case "parked": return .parked
        case "expired": return .expired
        case "deck": return .deck(skip: 0)
        case "confirm": return .confirm
        case "applying": return .applying
        case "finish": return .finish
        case "finish_running": return .finishRunning
        case "finish_needs": return .finishNeeds
        default: return nil
        }
    }()

    static let smartClear: SmartClear? = {
        switch ProcessInfo.processInfo.environment["PASSBAND_SMART_CLEAR_FIXTURE"] {
        case "working": return .working
        case "receipt": return .receipt
        case "list": return .list
        default: return nil
        }
    }()

    /// `PASSBAND_PURGE_RULES=<open|loading|none|other>`: the sender page
    /// comes up with "Create rule" already pressed. `loading` holds the
    /// suggestions in flight, `none` has the daemon suggest nothing (only
    /// Other is offered), `other` opens the field with a sentence typed in.
    enum Rules: Equatable { case open, loading, none, other }
    static let rules: Rules? = {
        switch ProcessInfo.processInfo.environment["PASSBAND_PURGE_RULES"] {
        case "open": return .open
        case "loading": return .loading
        case "none": return Rules.none
        case "other": return .other
        default: return nil
        }
    }()
    static let typedRule = "just the receipts, and anything about a return"

    /// `PASSBAND_PURGE_BODY=<loading|failed>`: every rendered email stays
    /// loading, or fails to load. `PASSBAND_PURGE_SELECT=N` comes up with the
    /// Nth email in the strip selected.
    enum BodyMode: Equatable { case loading, failed }
    static let body: BodyMode? = {
        switch ProcessInfo.processInfo.environment["PASSBAND_PURGE_BODY"] {
        case "loading": return .loading
        case "failed": return .failed
        default: return nil
        }
    }()
    static let selected: Int =
        Int(ProcessInfo.processInfo.environment["PASSBAND_PURGE_SELECT"] ?? "") ?? 0

    /// A canned email body, in Foundation's terms (the store turns it into
    /// its own).
    enum Body: Equatable {
        case html(String)
        case text(String)
        case failed
    }

    /// What "the daemon" returns for one fixture email: a promo built of
    /// blocks, a long newsletter, a plain-text email, and one that fails.
    static func rendered(subject: String, messageId: Int) -> Body {
        if body == .failed { return .failed }
        switch messageId / 100 {
        case 90: return .html(promo(subject))
        case 91: return .html(newsletter(subject))
        case 96: return .text(plain(subject))
        case 98: return .failed
        default: return .html(simple(subject))
        }
    }

    private static func promo(_ subject: String) -> String {
        let tiles = [("#F4D35E", "Patio sets", "from $249"), ("#EE964B", "Grills", "up to 40% off"),
            ("#8FB8DE", "Lighting", "2 for 1"), ("#9BC1A0", "Planters", "from $12")]
            .map { color, name, price in
                """
                <td width="25%" style="padding:6px"><div style="background:\(color);height:110px;border-radius:8px"></div>
                <div style="font:600 13px Helvetica,Arial,sans-serif;margin-top:8px">\(name)</div>
                <div style="font:12px Helvetica,Arial,sans-serif;color:#666">\(price)</div></td>
                """
            }.joined()
        return """
            <html><body style="margin:0;background:#ffffff;color:#111">
            <table width="100%" cellpadding="0" cellspacing="0" style="max-width:640px;margin:0 auto">
            <tr><td style="background:#C8102E;color:#fff;padding:14px 20px;font:700 20px Helvetica,Arial,sans-serif;letter-spacing:1px">DEAL STREAM</td></tr>
            <tr><td style="background:#1B1B3A;color:#fff;padding:44px 24px;text-align:center">
              <div style="font:800 34px Helvetica,Arial,sans-serif">48 HOURS ONLY</div>
              <div style="font:16px Helvetica,Arial,sans-serif;margin-top:8px;opacity:.85">\(subject)</div>
              <a href="https://example.com/shop" style="display:inline-block;margin-top:20px;background:#F4D35E;color:#111;padding:12px 28px;border-radius:24px;font:700 14px Helvetica,Arial,sans-serif;text-decoration:none">Shop the sale</a>
            </td></tr>
            <tr><td style="padding:14px"><table width="100%" cellpadding="0" cellspacing="0"><tr>\(tiles)</tr></table></td></tr>
            <tr><td style="padding:8px 20px 20px;font:13px/1.5 Helvetica,Arial,sans-serif;color:#444">Members who check out before midnight get an extra 10% and free delivery on orders over $75. Prices as marked. While supplies last.</td></tr>
            <tr><td style="background:#f3f3f3;padding:16px 20px;font:11px Helvetica,Arial,sans-serif;color:#888">You are receiving this because you shopped with us. <a href="https://example.com/unsubscribe" style="color:#888">Unsubscribe</a></td></tr>
            </table></body></html>
            """
    }

    private static func newsletter(_ subject: String) -> String {
        let paragraph =
            "The three of them never met, and for most of their lives did not know the others were writing at all. What survives is a set of letters to a shared editor, who kept every one and answered none at length. Reading them in order is a little like listening to three people talk in adjoining rooms."
        let body = (0..<9).map { _ in "<p style=\"margin:0 0 16px\">\(paragraph)</p>" }.joined()
        return """
            <html><body style="margin:0;background:#fffdf8;color:#222">
            <div style="max-width:600px;margin:0 auto;padding:32px 24px;font:17px/1.6 Georgia,serif">
            <div style="font:600 12px Helvetica,Arial,sans-serif;letter-spacing:2px;color:#9a6b2f">THE QUARTERLY REVIEW</div>
            <h1 style="font:700 28px/1.2 Georgia,serif;margin:10px 0 20px">\(subject)</h1>
            \(body)
            <p style="font:13px Helvetica,Arial,sans-serif;color:#888">Manage your subscription on <a href="https://example.com/account" style="color:#888">our site</a>.</p>
            </div></body></html>
            """
    }

    private static func plain(_ subject: String) -> String {
        """
        \(subject)

        Dear cardholder,

        This month your Harbor Bank Rewards card earns 5x points at restaurants, cafes and food delivery. No enrolment is needed.

        Points post within two statement cycles. See your account for the full terms.

        Harbor Bank Rewards
        """
    }

    private static func simple(_ subject: String) -> String {
        """
        <html><body style="margin:0;background:#ffffff;color:#222">
        <div style="max-width:560px;margin:0 auto;padding:28px 22px;font:15px/1.55 Helvetica,Arial,sans-serif">
        <h2 style="margin:0 0 14px;font:700 22px Helvetica,Arial,sans-serif">\(subject)</h2>
        <p>Here is what is new this week, picked for you.</p>
        <p><a href="https://example.com/offers" style="color:#2B7FD4">See everything</a></p>
        </div></body></html>
        """
    }

    /// True for the whole life of a process launched with either variable.
    static var active: Bool { purge != nil || smartClear != nil }

    static let ledgerDays = 34

    /// What the daemon's page "says": every sender below, or with `few`
    /// only two of the ones the deck would take.
    static var candidates: [PurgeSender] {
        purge == .few ? Array(wanted.prefix(2)) + unwanted : wanted + unwanted
    }

    /// What each deck sender is decided as when a fixture needs a deck
    /// already walked, in deck order.
    static let staged: [PurgeDecision] = [
        .unsubscribe, .unsubscribe, .block,
        .rule("Only order confirmations and delivery updates"), .keep, .ownRule(typedRule),
    ]

    private static func signals(
        received: Int, opened: Int, clearedUnopened: Int = 0, dwellMs: Int? = nil,
        clicks: Int = 0, readFraction: Double? = nil, words: Int? = nil, perMonth: Double
    ) -> PurgeSender.Signals {
        PurgeSender.Signals(
            received: received, opened: opened, cleared_unopened: clearedUnopened,
            dwell_sample: opened, median_dwell_ms: dwellMs, clicks: clicks, read_sample: opened,
            median_read_fraction: readFraction, median_words: words, per_month: perMonth)
    }

    /// The six the deck should hold, in the daemon's order.
    static let wanted: [PurgeSender] = [
        // A link the daemon can use, and enough mail that the list scrolls.
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
            open_count: 2, open_message_ids: [9101, 9102],
            signals: signals(
                received: 12, opened: 3, dwellMs: 6_000, readFraction: 0.08, words: 2_400,
                perMonth: 10.4),
            unsubscribe: .init(message_id: 9102, auto: false), score: 82,
            reasons: [
                .skimmed(medianReadFraction: 0.08, medianWords: 2_400, medianDwellMs: 6_000),
                .rarelyOpened(opened: 3, received: 12),
            ]),
        // No link anywhere: the way out is a block.
        PurgeSender(
            sender: "rewards@harborbank.example", display_name: "Harbor Bank Rewards",
            open_count: 3, open_message_ids: [9601, 9602, 9603],
            signals: signals(received: 41, opened: 0, clearedUnopened: 12, perMonth: 31.2),
            score: 88,
            reasons: [
                .rarelyOpened(opened: 0, received: 41), .highVolume(perMonth: 31.2),
                .clearedUnopened(count: 12),
            ]),
        // Part of what they send is worth keeping: three rules suggested.
        PurgeSender(
            sender: "hello@trailhead-outfitters.example", display_name: "Trailhead Outfitters",
            open_count: 4, open_message_ids: Array(9801...9804),
            signals: signals(
                received: 19, opened: 4, clearedUnopened: 6, dwellMs: 1_400, perMonth: 16.8),
            unsubscribe: .init(message_id: 9804, auto: true), score: 77,
            reasons: [.rarelyOpened(opened: 4, received: 19), .briefLooks(medianDwellMs: 1_400)]),
        // Already muted, with a link: no rules are suggested for it.
        PurgeSender(
            sender: "perks@parcelperks.example", display_name: "Parcel Perks",
            open_count: 3, open_message_ids: [9201, 9202, 9203],
            signals: signals(
                received: 11, opened: 2, clearedUnopened: 9, dwellMs: 900, perMonth: 9.6),
            rule: .init(id: 7, disposition: .squelch, match_pattern: "perks@parcelperks.example"),
            unsubscribe: .init(message_id: 9203, auto: true), score: 75,
            reasons: [.clearedUnopened(count: 9), .briefLooks(medianDwellMs: 900)]),
        PurgeSender(
            sender: "spin@couponcarousel.example", display_name: "Coupon Carousel",
            open_count: 2, open_message_ids: [9901, 9902],
            signals: signals(received: 14, opened: 0, clearedUnopened: 5, perMonth: 12.4),
            unsubscribe: .init(message_id: 9902, auto: true), score: 71,
            reasons: [.rarelyOpened(opened: 0, received: 14), .clearedUnopened(count: 5)]),
    ]

    /// Senders the daemon lists and the deck must NOT show: worth keeping,
    /// unsure, unknown, or with nothing left to offer.
    static let unwanted: [PurgeSender] = [
        // Read closely, and a score pushed high by volume alone.
        PurgeSender(
            sender: "notes@fieldnotes.example", display_name: "Field Notes Weekly",
            open_count: 3, open_message_ids: [9301, 9302, 9303],
            signals: signals(
                received: 9, opened: 8, dwellMs: 240_000, clicks: 5, readFraction: 0.92,
                words: 1_800, perMonth: 4.3),
            unsubscribe: .init(message_id: 9303, auto: true), score: 90,
            reasons: [.readClosely(medianReadFraction: 0.92), .highVolume(perMonth: 4.3)]),
        // Too new to say anything about.
        PurgeSender(
            sender: "team@nimbus.example", display_name: "Nimbus",
            open_count: 1, open_message_ids: [9401],
            signals: signals(received: 1, opened: 0, perMonth: 0.9),
            score: nil, reasons: [.notEnoughHistory]),
        // Middling: unsure.
        PurgeSender(
            sender: "hello@ox.example", display_name: "Ox",
            open_count: 4, open_message_ids: Array(9501...9504),
            signals: signals(received: 9, opened: 4, dwellMs: 3_000, perMonth: 7.8),
            unsubscribe: .init(message_id: 9504, auto: true), score: 69,
            reasons: [.rarelyOpened(opened: 4, received: 9), .briefLooks(medianDwellMs: 3_000)]),
        // Weak evidence.
        PurgeSender(
            sender: "ada@morningswithada.example", display_name: "Mornings with Ada",
            open_count: 2, open_message_ids: [9701, 9702],
            signals: signals(received: 6, opened: 5, dwellMs: 300, perMonth: 5.2),
            unsubscribe: .init(message_id: 9702, auto: true), score: 30,
            reasons: [.briefLooks(medianDwellMs: 300), .neverClicked(opened: 5)]),
        // Damning, but no link and already muted: nothing left to offer.
        PurgeSender(
            sender: "no-reply@loudmart.example", display_name: "Loudmart",
            open_count: 6, open_message_ids: Array(9951...9956),
            signals: signals(received: 30, opened: 0, clearedUnopened: 20, perMonth: 26),
            rule: .init(id: 9, disposition: .squelch, match_pattern: "no-reply@loudmart.example"),
            score: 97, reasons: [.rarelyOpened(opened: 0, received: 30)]),
    ]

    /// The narrowing rules "the daemon" suggests, by sender. A sender with
    /// no entry gets none, and its page draws no suggestions section.
    static let suggestions: [String: [String]] = [
        "deals@dealstream.example": [
            "Only order confirmations and delivery updates",
            "Only price drops on things already in your cart",
            "Only the weekly roundup, not the daily deals",
        ],
        "rewards@harborbank.example": [
            "Only statements and changes to your account",
            "Only offers that expire within a week",
        ],
        "hello@trailhead-outfitters.example": [
            "Only order confirmations and delivery updates",
            "Only sales on tents, sleeping bags and other camping gear that you have looked at before",
            "Only store events near you",
        ],
    ]

    private static func ago(_ hours: Double) -> String {
        ISO8601DateFormatter().string(from: Date().addingTimeInterval(-hours * 3600))
    }

    private static func mail(_ base: Int, _ lines: [String]) -> [AttentionUpdate] {
        lines.enumerated().map { i, line in
            AttentionUpdate(
                id: base + i, thread_id: "fixture-\(base + i)", tier: .noise, importance: 10,
                sender: "fixture@example.com", one_line: line, reason: "", subject: line,
                status: .open, received_at: ago(Double(i) * 31 + 3), placement: "reading")
        }
    }

    /// Each sender's emails in Reading, newest first, standing in for the
    /// board the real page reads them from.
    static var emails: [String: [AttentionUpdate]] {
        [
            "deals@dealstream.example": mail(
                9001,
                [
                    "48 hours only: up to 60% off patio furniture, grills and outdoor lighting, plus free delivery on orders over $75 and an extra 10% for members who check out before midnight",
                    "Your Tuesday deals are here", "Flash sale on headphones ends tonight",
                    "Last chance for free shipping", "Monday markdowns: kitchen and home",
                    "The weekend sale starts now", "Back in stock and already going fast",
                    "Your Friday deals are here", "Doorbusters: 24 hours only",
                    "New arrivals picked for you", "Thursday: up to 40% off luggage",
                    "A coupon is waiting in your account", "Midweek markdowns",
                    "Your Tuesday deals are here",
                ]),
            "editors@quarterly-review-of-books.example": mail(
                9101,
                [
                    "Autumn issue: a very long essay on the letters of three novelists who never met",
                    "From the archive: on rereading",
                ]),
            "rewards@harborbank.example": mail(
                9601,
                [
                    "Earn 5x points on dining this month", "Your rewards summary",
                    "A new offer is waiting in your account",
                ]),
            "hello@trailhead-outfitters.example": mail(
                9801,
                [
                    "The fall sale: 30% off tents and packs", "Your order has shipped",
                    "Trail notes: five ridge walks for October", "Members get early access",
                ]),
            "perks@parcelperks.example": mail(
                9201,
                ["You have 240 points waiting", "Double points this weekend", "Rate your delivery"]),
            "spin@couponcarousel.example": mail(
                9901, ["Spin to win: today's coupon", "Your coupon expires tonight"]),
        ]
    }

    // MARK: smart clear

    static var smartClearResult: SmartClearResult {
        func row(_ id: Int, _ reason: String, _ sender: String, _ name: String, _ subject: String, _ hours: Double)
            -> SmartClearResult.Row
        {
            .init(
                message_id: id, reason: reason, sender: sender, sender_name: name,
                subject: subject, received_at: ago(hours))
        }
        let rows: [SmartClearResult.Row] = [
            row(1, "event_passed", "events@cityhall.example", "City Hall Events", "Tonight: the harbor concert series", 80),
            row(2, "event_passed", "events@cityhall.example", "City Hall Events", "Saturday farmers market, new hours", 130),
            row(3, "event_passed", "hello@webinars.example", "Launch Webinars", "Starting in one hour: scaling your pipeline", 52),
            row(4, "offer_ended", "deals@dealstream.example", "Deal Stream Daily", "48 hours only: up to 60% off patio furniture, grills and outdoor lighting, plus free delivery on orders over $75 and an extra 10% for members", 100),
            row(5, "offer_ended", "deals@dealstream.example", "Deal Stream Daily", "Flash sale on headphones ends tonight", 150),
            row(6, "offer_ended", "spin@couponcarousel.example", "Coupon Carousel", "Your coupon expires tonight", 60),
            row(7, "offer_ended", "hello@trailhead-outfitters.example", "Trailhead Outfitters", "Last day: 30% off tents and packs", 75),
            row(8, "offer_ended", "air@skyfare.example", "Skyfare", "Fares to Lisbon from $389, book by Friday", 200),
            row(9, "expired", "status@cloudhost.example", "Cloudhost Status", "Scheduled maintenance this Sunday", 240),
            row(10, "expired", "news@thedailybrief.example", "The Daily Brief", "Monday: what to watch this week", 170),
            row(11, "expired", "news@thedailybrief.example", "The Daily Brief", "Friday: the week in five charts", 260),
            row(12, "expired", "polls@townsquare.example", "Town Square", "Voting closes tomorrow on the park plan", 300),
            row(13, "ignored_sender", "rewards@harborbank.example", "Harbor Bank Rewards", "Earn 5x points on dining this month", 20),
            row(14, "ignored_sender", "rewards@harborbank.example", "Harbor Bank Rewards", "Your rewards summary", 190),
            row(15, "ignored_sender", "rewards@harborbank.example", "Harbor Bank Rewards", "A new offer is waiting in your account", 400),
            row(16, "ignored_sender", "editors@quarterly-review-of-books.example", "The Extremely Long Named Quarterly Review of Books and Letters", "From the archive: on rereading", 90),
            row(17, "ignored_sender", "no-reply@loudmart.example", "Loudmart", "Rollbacks you will love", 30),
            row(18, "ignored_sender", "no-reply@loudmart.example", "Loudmart", "Your cart misses you", 45),
            row(19, "stale_promo", "perks@parcelperks.example", "Parcel Perks", "Double points this weekend", 210),
            row(20, "stale_promo", "perks@parcelperks.example", "Parcel Perks", "You have 240 points waiting", 500),
            row(21, "stale_promo", "style@threadline.example", "Threadline", "New season, new layers", 330),
            row(22, "stale_promo", "style@threadline.example", "Threadline", "The linen edit", 600),
            row(23, "stale_promo", "eat@supperclub.example", "Supper Club", "This week's menu is live", 280),
            row(24, "from_a_newer_daemon", "hello@ox.example", "Ox", "New in the shop this week", 310),
        ]
        return SmartClearResult(
            batch_id: "fixture", cleared: 43, senders: 17,
            by_reason: [
                "event_passed": 3, "offer_ended": 5, "expired": 4, "ignored_sender": 20,
                "stale_promo": 11,
            ], message_ids: [], rows: rows)
    }
}
