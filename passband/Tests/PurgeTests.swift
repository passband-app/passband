// The purge deck: who is offered, what each card may be asked, what is
// suggested, what a set of decisions comes to, and the sentences the evidence
// is put in. Everything here ends as either a request to a third party that
// cannot be recalled or a claim about somebody's own reading, so each rule is
// pinned rather than read.

import Foundation

@main
struct PurgeTests {
    static func main() throws {
        try decodesTheDaemonsPage()
        try anUnknownReasonIsDroppedNotFatal()
        try aKnownReasonMissingANumberIsDropped()
        aKnownContactIsNeverOnTheDeck()
        aSurfaceRuleKeepsASenderOffTheDeck()
        anAlreadyAskedSenderIsOffTheDeck()
        aRecentlyKeptSenderIsOffTheDeck()
        try decodesKeptAtAndTheTotal()
        siteBoundIsTheStagedUnsubscribesTheDaemonWillNotDo()
        theStagedSummarySplitsOutTheirSite()
        theCommittedSummarySplitsOutTheirSite()
        theDeckKeepsTheDaemonsOrder()
        unsubscribeNeedsALink()
        aMutedSenderIsNotOfferedMute()
        aHighScoreSuggestsEndingIt()
        aHighScoreWithNoWayOutSuggestsClearing()
        aMiddlingScoreSuggestsClearing()
        aLowOrMissingScoreSuggestsNothing()
        readCloselyNeverSuggestsLeaving()
        decidingStagesAndAdvances()
        aDisallowedDecisionIsRefused()
        backUnstagesThePreviousCard()
        finishingEarlyLeavesTheRestUntouched()
        progressIsAPositionThatStopsAtTheTotal()
        theStagedSummaryAddsUp()
        aSummaryOfSkipsIsEmpty()
        perMonthIsRoundedOnce()
        theCommittedSummaryCountsOnlyWhatLanded()
        everyReasonHasItsSentence()
        durationsReadAsSecondsThenMinutes()
        wordsAreRoundedToWhatSomeoneWouldSay()
        volumeRoundsAndNeverSaysZero()
        theWindowIsTheLedgersDays()
        noSentenceCarriesADash()
        print("purge: ok")
    }

    // MARK: fixtures

    static func signals(perMonth: Double = 4) -> PurgeSender.Signals {
        PurgeSender.Signals(
            received: 12, opened: 1, cleared_unopened: 3, dwell_sample: 1, median_dwell_ms: 4_000,
            clicks: 0, read_sample: 1, median_read_fraction: 0.1, median_words: 800,
            per_month: perMonth)
    }

    static func sender(
        _ address: String, open: Int = 2, perMonth: Double = 4, known: Bool = false,
        rule: Disposition? = nil, link: Bool = true, auto: Bool = true, asked: Bool = false,
        score: Int? = 80,
        reasons: [PurgeReason] = [.rarelyOpened(opened: 1, received: 12)]
    ) -> PurgeSender {
        PurgeSender(
            sender: address, display_name: address, open_count: open,
            open_message_ids: Array((0..<open).map { 100 - $0 }),
            signals: signals(perMonth: perMonth), known_contact: known,
            rule: rule.map { PurgeSender.Rule(id: 1, disposition: $0, match_pattern: address) },
            unsubscribe: link ? PurgeSender.Unsubscribe(message_id: 100, auto: auto) : nil,
            unsubscribed: asked
                ? PurgeSender.Unsubscribed(status: "done", requested_at: "2026-10-01T00:00:00Z") : nil,
            score: score, reasons: reasons)
    }

    static func card(_ s: PurgeSender) -> PurgeCard {
        guard let card = PurgeCard(s) else { preconditionFailure("\(s.sender) should be on the deck") }
        return card
    }

    static func decode(_ json: String) throws -> ReadingSendersPage {
        try JSONDecoder().decode(ReadingSendersPage.self, from: Data(json.utf8))
    }

    static func page(reasons: String) -> String {
        """
        { "window_days": 90, "ledger_days": 34, "total_senders": 1, "senders": [ {
            "sender": "news@example.com", "display_name": "Example Weekly",
            "open_count": 2, "open_message_ids": [912, 880],
            "signals": { "received": 23, "opened": 1, "cleared_unopened": 9,
                "dwell_sample": 1, "median_dwell_ms": 4200, "clicks": 0,
                "read_sample": 1, "median_read_fraction": 0.04, "median_words": 900,
                "per_month": 20.3 },
            "known_contact": false,
            "rule": {"id": 4, "disposition": "filtered", "match_pattern": "*@example.com"},
            "unsubscribe": {"message_id": 912, "auto": true},
            "unsubscribed": null,
            "score": 88,
            "reasons": \(reasons)
        } ] }
        """
    }

    // MARK: the wire

    static func decodesTheDaemonsPage() throws {
        let all = """
            [ {"code": "rarely_opened", "opened": 1, "received": 23},
              {"code": "cleared_unopened", "count": 9},
              {"code": "skimmed", "median_read_fraction": 0.04, "median_words": 900, "median_dwell_ms": 4200},
              {"code": "brief_looks", "median_dwell_ms": 2100},
              {"code": "never_clicked", "opened": 1},
              {"code": "high_volume", "per_month": 20.3},
              {"code": "read_closely", "median_read_fraction": 0.9},
              {"code": "known_contact"}, {"code": "wanted_by_rule"}, {"code": "not_enough_history"},
              {"code": "kept_recently"} ]
            """
        let got = try decode(page(reasons: all))
        precondition(got.window_days == 90 && got.ledger_days == 34)
        precondition(got.senders[0].kept_at == nil, "absent is nil")
        let s = got.senders[0]
        precondition(s.sender == "news@example.com" && s.display_name == "Example Weekly")
        precondition(s.open_count == 2 && s.open_message_ids == [912, 880])
        precondition(s.signals.received == 23 && s.signals.median_dwell_ms == 4200)
        precondition(s.signals.per_month == 20.3 && s.signals.median_words == 900)
        precondition(s.rule?.disposition == .filtered && s.rule?.id == 4)
        precondition(s.unsubscribe == PurgeSender.Unsubscribe(message_id: 912, auto: true))
        precondition(s.unsubscribed == nil && s.score == 88)
        precondition(
            s.reasons == [
                .rarelyOpened(opened: 1, received: 23), .clearedUnopened(count: 9),
                .skimmed(medianReadFraction: 0.04, medianWords: 900, medianDwellMs: 4200),
                .briefLooks(medianDwellMs: 2100), .neverClicked(opened: 1),
                .highVolume(perMonth: 20.3), .readClosely(medianReadFraction: 0.9),
                .knownContact, .wantedByRule, .notEnoughHistory, .keptRecently,
            ], "every code maps to its case, in the daemon's order")

        // The nullable half: no rule, no link, an earlier request, no score.
        let bare = """
            { "window_days": 90, "ledger_days": 3, "senders": [ {
                "sender": "a@b.c", "display_name": "A", "open_count": 1, "open_message_ids": [5],
                "signals": { "received": 1, "opened": 0, "cleared_unopened": 0, "dwell_sample": 0,
                    "median_dwell_ms": null, "clicks": 0, "read_sample": 0,
                    "median_read_fraction": null, "median_words": null, "per_month": 0.5 },
                "known_contact": true, "rule": null, "unsubscribe": null,
                "unsubscribed": {"status": "browser", "requested_at": "2026-10-01T00:00:00Z"},
                "score": null, "reasons": [] } ] }
            """
        let b = try decode(bare).senders[0]
        precondition(b.known_contact && b.rule == nil && b.unsubscribe == nil && b.score == nil)
        precondition(b.unsubscribed?.status == "browser", "a status UnsubStatus has no case for")
        precondition(b.signals.median_dwell_ms == nil && b.signals.median_words == nil)
    }

    static func anUnknownReasonIsDroppedNotFatal() throws {
        let got = try decode(
            page(
                reasons: """
                    [ {"code": "opened_on_tuesdays", "tuesdays": 4},
                      {"code": "high_volume", "per_month": 20.3},
                      {"code": "another_new_one"} ]
                    """))
        precondition(
            got.senders[0].reasons == [.highVolume(perMonth: 20.3)],
            "a newer daemon's reason costs that one reason, never the deck")
    }

    static func aKnownReasonMissingANumberIsDropped() throws {
        let got = try decode(
            page(reasons: #"[ {"code": "rarely_opened", "opened": 1}, {"code": "never_clicked", "opened": 3} ]"#))
        precondition(got.senders[0].reasons == [.neverClicked(opened: 3)])
    }

    // MARK: who is on the deck

    static func aKnownContactIsNeverOnTheDeck() {
        precondition(PurgeCard(sender("mum@example.com", known: true, score: 99)) == nil)
        precondition(PurgeCard(sender("shop@example.com")) != nil)
    }

    static func aSurfaceRuleKeepsASenderOffTheDeck() {
        precondition(PurgeCard(sender("wanted@example.com", rule: .surface)) == nil)
        precondition(PurgeCard(sender("f@example.com", rule: .filtered)) != nil, "only surface excludes")
        precondition(PurgeCard(sender("m@example.com", rule: .squelch)) != nil)
    }

    static func anAlreadyAskedSenderIsOffTheDeck() {
        precondition(PurgeCard(sender("asked@example.com", asked: true)) == nil)
    }

    static func aRecentlyKeptSenderIsOffTheDeck() {
        precondition(PurgeCard(sender("kept@x.com", score: 0, reasons: [.keptRecently])) == nil)
        // The daemon's word, not a date: a `kept_at` it no longer calls
        // recent is a sender to ask about again.
        var old = sender("old@x.com")
        old.kept_at = "2026-10-04T00:00:00Z"
        precondition(PurgeCard(old) != nil, "kept_at alone decides nothing")
    }

    static func decodesKeptAtAndTheTotal() throws {
        let json = """
            { "window_days": 90, "ledger_days": 34, "total_senders": 140, "senders": [ {
                "sender": "a@b.c", "display_name": "A", "open_count": 1, "open_message_ids": [5],
                "signals": { "received": 9, "opened": 9, "cleared_unopened": 0, "dwell_sample": 0,
                    "median_dwell_ms": null, "clicks": 0, "read_sample": 0,
                    "median_read_fraction": null, "median_words": null, "per_month": 3 },
                "known_contact": false, "rule": null, "unsubscribe": null, "unsubscribed": null,
                "kept_at": "2026-09-20T10:00:00Z",
                "score": 0, "reasons": [ {"code": "kept_recently"} ] } ] }
            """
        let got = try decode(json)
        precondition(got.totalSenders == 140, "the daemon's count, not the page's")
        precondition(got.senders[0].kept_at == "2026-09-20T10:00:00Z")
        precondition(got.senders[0].reasons == [.keptRecently])
        precondition(PurgeDeck(senders: got.senders).isEmpty)
        // A daemon that does not say: what arrived.
        let quiet = try decode(page(reasons: "[]").replacingOccurrences(of: "\"total_senders\": 1,", with: ""))
        precondition(quiet.total_senders == nil && quiet.totalSenders == 1)
    }

    static func siteBoundIsTheStagedUnsubscribesTheDaemonWillNotDo() {
        precondition(!card(sender("a@x.com", auto: true)).unsubscribeIsOnTheirSite)
        precondition(card(sender("a@x.com", auto: false)).unsubscribeIsOnTheirSite)
        precondition(!card(sender("a@x.com", link: false)).unsubscribeIsOnTheirSite, "no link is not a site")
        var deck = PurgeDeck(senders: [
            sender("auto@x.com", auto: true), sender("site@x.com", auto: false),
            sender("muted@x.com", auto: false), sender("site2@x.com", auto: false),
            sender("later@x.com", auto: false),
        ])
        precondition(deck.siteBound.isEmpty, "nothing staged")
        deck.decide(.unsubscribe)
        deck.decide(.unsubscribe)
        deck.decide(.mute)  // a site sender, but not asked to unsubscribe
        deck.decide(.unsubscribe)
        precondition(deck.siteBound.map(\.id) == ["site@x.com", "site2@x.com"], "in deck order")
    }

    static func theStagedSummarySplitsOutTheirSite() {
        var deck = PurgeDeck(senders: [
            sender("auto@x.com", open: 3, perMonth: 10, auto: true),
            sender("site@x.com", open: 5, perMonth: 30, auto: false),
        ])
        deck.decide(.unsubscribe)
        deck.decide(.unsubscribe)
        let s = PurgeSummary(deck: deck)
        precondition(s.unsubscribed == 1 && s.onTheirSite == 1, "not claimed until it has happened")
        precondition(s.fewerPerMonth == 10, "the site sender's 30 a month is still arriving")
        precondition(s.emailsCleared == 8, "their open mail is cleared all the same")
        // Alone, it is still something to confirm: mail gets cleared.
        var only = PurgeDeck(senders: [sender("site@x.com", auto: false)])
        only.decide(.unsubscribe)
        precondition(!PurgeSummary(deck: only).isEmpty)
        // A MUTE of a site sender is a mute.
        var muted = PurgeDeck(senders: [sender("site@x.com", perMonth: 6, auto: false)])
        muted.decide(.mute)
        precondition(PurgeSummary(deck: muted).onTheirSite == 0 && PurgeSummary(deck: muted).fewerPerMonth == 6)
    }

    static func theCommittedSummarySplitsOutTheirSite() {
        var deck = PurgeDeck(senders: [
            sender("auto@x.com", open: 3, perMonth: 10, auto: true),
            sender("site@x.com", open: 5, perMonth: 30, auto: false),
            sender("refused@x.com", open: 2, perMonth: 8, auto: true),
        ])
        for _ in 0..<3 { deck.decide(.unsubscribe) }
        let s = PurgeSummary(
            deck: deck,
            outcomes: [
                "auto@x.com": .applied(.unsubscribe, emailsCleared: 3),
                "site@x.com": .onTheirSite(emailsCleared: 4),
                // Promised automatic, handed back a page at request time.
                "refused@x.com": .onTheirSite(emailsCleared: 2),
            ])
        precondition(s.unsubscribed == 1 && s.onTheirSite == 2 && s.failed == 0)
        precondition(s.fewerPerMonth == 10 && s.emailsCleared == 9)
        let none = PurgeSummary(deck: deck, outcomes: ["site@x.com": .onTheirSite(emailsCleared: 0)])
        precondition(!none.isEmpty && none.failed == 2)
    }

    static func theDeckKeepsTheDaemonsOrder() {
        let deck = PurgeDeck(senders: [
            sender("c@x.com", score: 90), sender("known@x.com", known: true),
            sender("a@x.com", score: 20), sender("b@x.com", score: 50),
        ])
        precondition(deck.cards.map(\.id) == ["c@x.com", "a@x.com", "b@x.com"], "filtered, not re-sorted")
        precondition(PurgeDeck(senders: [sender("k@x.com", known: true)]).isEmpty)
    }

    // MARK: what a card may be asked

    static func unsubscribeNeedsALink() {
        precondition(card(sender("a@x.com", link: true)).allowed == [.unsubscribe, .mute, .clear, .skip])
        precondition(card(sender("a@x.com", link: false)).allowed == [.mute, .clear, .skip])
    }

    static func aMutedSenderIsNotOfferedMute() {
        precondition(card(sender("a@x.com", rule: .squelch)).allowed == [.unsubscribe, .clear, .skip])
        precondition(card(sender("a@x.com", rule: .filtered)).allowed.contains(.mute))
    }

    // MARK: what is suggested

    static func aHighScoreSuggestsEndingIt() {
        precondition(card(sender("a@x.com", score: 70)).recommended == .unsubscribe, "70 is in")
        precondition(card(sender("a@x.com", link: false, score: 95)).recommended == .mute)
    }

    static func aHighScoreWithNoWayOutSuggestsClearing() {
        precondition(card(sender("a@x.com", rule: .squelch, link: false, score: 95)).recommended == .clear)
    }

    static func aMiddlingScoreSuggestsClearing() {
        precondition(card(sender("a@x.com", score: 69)).recommended == .clear)
        precondition(card(sender("a@x.com", score: 40)).recommended == .clear, "40 is in")
    }

    static func aLowOrMissingScoreSuggestsNothing() {
        precondition(card(sender("a@x.com", score: 39)).recommended == nil)
        precondition(card(sender("a@x.com", score: nil)).recommended == nil)
    }

    static func readCloselyNeverSuggestsLeaving() {
        let read: [PurgeReason] = [.readClosely(medianReadFraction: 0.9), .highVolume(perMonth: 30)]
        precondition(card(sender("a@x.com", score: 95, reasons: read)).recommended == .clear)
        precondition(card(sender("a@x.com", link: false, score: 95, reasons: read)).recommended == .clear)
        // Only as the TOP reason: further down it lost the argument.
        let buried: [PurgeReason] = [.highVolume(perMonth: 30), .readClosely(medianReadFraction: 0.9)]
        precondition(card(sender("a@x.com", score: 95, reasons: buried)).recommended == .unsubscribe)
    }

    // MARK: the deck

    static func three() -> PurgeDeck {
        PurgeDeck(senders: [
            sender("a@x.com", open: 3, perMonth: 10), sender("b@x.com", open: 5, perMonth: 2.4, link: false),
            sender("c@x.com", open: 1, perMonth: 7),
        ])
    }

    static func decidingStagesAndAdvances() {
        var deck = three()
        precondition(deck.current?.id == "a@x.com" && !deck.isFinished && deck.staged.isEmpty)
        precondition(deck.decide(.unsubscribe))
        precondition(deck.current?.id == "b@x.com" && deck.decision(for: "a@x.com") == .unsubscribe)
        precondition(deck.decide(.skip) && deck.decide(.clear))
        precondition(deck.isFinished && deck.current == nil)
        precondition(deck.plan.map(\.card.id) == ["a@x.com", "b@x.com", "c@x.com"])
        precondition(deck.plan.map(\.decision) == [.unsubscribe, .skip, .clear])
        precondition(!deck.decide(.skip), "nothing to decide past the end")
        precondition(deck.staged.count == 3)
    }

    static func aDisallowedDecisionIsRefused() {
        var deck = three()
        deck.decide(.skip)
        let before = deck
        precondition(!deck.decide(.unsubscribe), "b has no link")
        precondition(deck == before, "a refusal stages nothing and does not advance")
        precondition(deck.decide(.mute))
    }

    static func backUnstagesThePreviousCard() {
        var deck = three()
        precondition(!deck.back(), "nothing behind the first card")
        deck.decide(.unsubscribe)
        deck.decide(.mute)
        precondition(deck.back())
        precondition(deck.current?.id == "b@x.com")
        precondition(deck.decision(for: "b@x.com") == nil, "asked again, not shown pre-answered")
        precondition(deck.decision(for: "a@x.com") == .unsubscribe, "only the one stepped onto")
        // From the far end.
        deck.decide(.mute)
        deck.decide(.clear)
        precondition(deck.isFinished && deck.back() && !deck.isFinished)
        precondition(deck.current?.id == "c@x.com" && deck.decision(for: "c@x.com") == nil)
    }

    static func finishingEarlyLeavesTheRestUntouched() {
        var deck = three()
        deck.decide(.mute)
        deck.finishEarly()
        precondition(deck.isFinished && deck.current == nil)
        precondition(deck.staged == ["a@x.com": .mute], "b and c were never looked at")
        precondition(!deck.decide(.clear), "a left deck takes no more decisions")
        let summary = PurgeSummary(deck: deck)
        precondition(summary.muted == 1 && summary.skipped == 0, "untouched is not skipped")
        // Back from the confirm step: the card they left on, nothing undone.
        precondition(deck.back())
        precondition(!deck.isFinished && deck.current?.id == "b@x.com")
        precondition(deck.staged == ["a@x.com": .mute])
    }

    static func progressIsAPositionThatStopsAtTheTotal() {
        var deck = three()
        precondition(deck.progress == PurgeDeck.Progress(position: 1, total: 3))
        deck.decide(.skip)
        precondition(deck.progress == PurgeDeck.Progress(position: 2, total: 3))
        deck.decide(.skip)
        deck.decide(.skip)
        precondition(deck.progress == PurgeDeck.Progress(position: 3, total: 3), "never 4 of 3")
    }

    // MARK: the summary

    static func theStagedSummaryAddsUp() {
        var deck = PurgeDeck(senders: [
            sender("a@x.com", open: 3, perMonth: 10), sender("b@x.com", open: 5, perMonth: 2.4),
            sender("c@x.com", open: 1, perMonth: 7), sender("d@x.com", open: 40, perMonth: 99),
            sender("e@x.com", open: 2, perMonth: 1),
        ])
        deck.decide(.unsubscribe)
        deck.decide(.mute)
        deck.decide(.clear)
        deck.decide(.skip)
        deck.decide(.unsubscribe)
        let s = PurgeSummary(deck: deck)
        precondition(s.unsubscribed == 2 && s.muted == 1 && s.cleared == 1 && s.skipped == 1)
        precondition(s.failed == 0)
        precondition(s.emailsCleared == 3 + 5 + 1 + 2, "every decision but the skip's 40")
        precondition(s.fewerPerMonth == 13, "10 + 2.4 + 1; not the cleared 7, not the skipped 99")
        precondition(!s.isEmpty)
    }

    static func aSummaryOfSkipsIsEmpty() {
        var deck = three()
        precondition(PurgeSummary(deck: deck).isEmpty, "nothing decided")
        deck.decide(.skip)
        deck.decide(.skip)
        precondition(PurgeSummary(deck: deck).isEmpty && PurgeSummary(deck: deck).skipped == 2)
        deck.decide(.clear)
        precondition(!PurgeSummary(deck: deck).isEmpty, "a clear alone is something to confirm")
    }

    static func perMonthIsRoundedOnce() {
        var deck = PurgeDeck(senders: (0..<5).map { sender("s\($0)@x.com", perMonth: 0.4) })
        for _ in 0..<5 { deck.decide(.mute) }
        precondition(PurgeSummary(deck: deck).fewerPerMonth == 2, "five at 0.4 are two, not five zeros")
        // To the NEAREST: 2.6 fewer is three, where cutting the fraction off
        // (or rounding each 1.3 down first) would say two.
        var pair = PurgeDeck(senders: (0..<2).map { sender("p\($0)@x.com", perMonth: 1.3) })
        for _ in 0..<2 { pair.decide(.unsubscribe) }
        precondition(PurgeSummary(deck: pair).fewerPerMonth == 3)
    }

    static func theCommittedSummaryCountsOnlyWhatLanded() {
        var deck = PurgeDeck(senders: [
            sender("a@x.com", open: 3, perMonth: 10), sender("b@x.com", open: 5, perMonth: 4),
            sender("c@x.com", open: 6, perMonth: 7), sender("d@x.com", open: 2, perMonth: 3),
            sender("e@x.com", open: 9, perMonth: 50), sender("f@x.com", open: 1, perMonth: 1),
        ])
        deck.decide(.unsubscribe)  // lands
        deck.decide(.unsubscribe)  // no link after all: muted instead
        deck.decide(.clear)  // 4 of its 6 marks landed
        deck.decide(.mute)  // fails
        deck.decide(.skip)
        deck.decide(.unsubscribe)  // never tried
        let s = PurgeSummary(
            deck: deck,
            outcomes: [
                "a@x.com": .applied(.unsubscribe, emailsCleared: 3),
                "b@x.com": .applied(.mute, emailsCleared: 5),
                "c@x.com": .applied(.clear, emailsCleared: 4),
                "d@x.com": .failed,
            ])
        precondition(s.unsubscribed == 1 && s.muted == 1 && s.cleared == 1, "counted as what was DONE")
        precondition(s.skipped == 1 && s.failed == 2, "the failure and the one never tried")
        precondition(s.emailsCleared == 3 + 5 + 4)
        precondition(s.fewerPerMonth == 14, "a and b; not the failed d or the untried f")
        precondition(PurgeSummary(deck: deck, outcomes: [:]).isEmpty, "nothing landed")
        precondition(PurgeSummary(deck: deck, outcomes: [:]).failed == 5)
    }

    // MARK: the sentences

    static let everyReason: [(PurgeReason, String)] = [
        (.rarelyOpened(opened: 1, received: 23), "Opened 1 of 23"),
        (.rarelyOpened(opened: 0, received: 23), "Opened none of 23"),
        (.clearedUnopened(count: 9), "Cleared 9 without opening"),
        (.clearedUnopened(count: 1), "Cleared 1 without opening"),
        (
            .skimmed(medianReadFraction: 0.04, medianWords: 900, medianDwellMs: 6_200),
            "About 900 words each, read for 6 seconds"
        ),
        (
            .skimmed(medianReadFraction: 0.01, medianWords: 2_430, medianDwellMs: 1_000),
            "About 2,450 words each, read for 1 second"
        ),
        (.briefLooks(medianDwellMs: 2_100), "Opened for about 2 seconds each"),
        (.briefLooks(medianDwellMs: 1_200), "Opened for about 1 second each"),
        (.briefLooks(medianDwellMs: 300), "Opened for less than a second each"),
        (.neverClicked(opened: 12), "Never clicked a link"),
        (.highVolume(perMonth: 20.3), "About 20 a month"),
        (.readClosely(medianReadFraction: 0.9), "You read most of each one"),
        (.knownContact, "You write to this address"),
        (.wantedByRule, "One of your rules asks for this sender"),
        (.notEnoughHistory, "Not enough history yet"),
        (.keptRecently, "You chose to keep these"),
    ]

    static func everyReasonHasItsSentence() {
        for (reason, want) in everyReason {
            let got = PurgeCopy.sentence(reason)
            precondition(got == want, "\(reason): got \"\(got)\", want \"\(want)\"")
        }
    }

    static func durationsReadAsSecondsThenMinutes() {
        let cases: [(Int, String)] = [
            (0, "less than a second"), (499, "less than a second"), (500, "1 second"),
            (1_499, "1 second"), (1_500, "2 seconds"), (59_400, "59 seconds"),
            (59_600, "1 minute"), (60_000, "1 minute"), (89_000, "1 minute"),
            (90_000, "2 minutes"), (600_000, "10 minutes"), (-5, "less than a second"),
        ]
        for (ms, want) in cases {
            precondition(PurgeCopy.duration(ms: ms) == want, "\(ms)ms: \(PurgeCopy.duration(ms: ms))")
        }
    }

    static func wordsAreRoundedToWhatSomeoneWouldSay() {
        let cases: [(Int, String)] = [
            (0, "0 words"), (1, "1 word"), (7, "7 words"), (19, "19 words"), (24, "20 words"),
            (126, "130 words"), (200, "200 words"), (224, "200 words"), (225, "250 words"),
            (874, "850 words"), (875, "900 words"), (999, "1,000 words"), (12_340, "12,350 words"),
        ]
        for (n, want) in cases {
            precondition(PurgeCopy.words(n) == want, "\(n): \(PurgeCopy.words(n))")
        }
    }

    static func volumeRoundsAndNeverSaysZero() {
        precondition(PurgeCopy.sentence(.highVolume(perMonth: 0.3)) == "Less than 1 a month")
        precondition(PurgeCopy.sentence(.highVolume(perMonth: 0.5)) == "About 1 a month")
        precondition(PurgeCopy.sentence(.highVolume(perMonth: 1.4)) == "About 1 a month")
        precondition(PurgeCopy.sentence(.highVolume(perMonth: 29.5)) == "About 30 a month")
    }

    static func theWindowIsTheLedgersDays() {
        precondition(PurgeCopy.window(ledgerDays: 34) == "in the last 34 days")
        precondition(PurgeCopy.window(ledgerDays: 2) == "in the last 2 days")
        precondition(PurgeCopy.window(ledgerDays: 1) == "in the last day")
        precondition(PurgeCopy.window(ledgerDays: 0) == "in the last day", "never \"0 days\"")
    }

    /// The product's copy rule, asserted over everything this file can say.
    static func noSentenceCarriesADash() {
        let said =
            everyReason.map { PurgeCopy.sentence($0.0) }
            + [0, 1, 34].map { PurgeCopy.window(ledgerDays: $0) }
        for line in said {
            precondition(!line.contains("\u{2014}") && !line.contains("\u{2013}"), line)
        }
    }
}
