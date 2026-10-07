// The purge deck: who is offered (only the senders the evidence is confident
// about), what each may be asked, what a set of decisions comes to, how the
// confirm page groups them, and the sentences all of it is put in. Everything
// here ends as either a request to a third party that cannot be recalled or a
// claim about somebody's own reading, so each rule is pinned rather than read.

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
        everyReasonHasItsSentence()
        durationsReadAsSecondsThenMinutes()
        wordsAreRoundedToWhatSomeoneWouldSay()
        volumeRoundsAndNeverSaysZero()
        theWindowIsTheLedgersDays()
        noSentenceCarriesADash()
        marksAreOneEachUntilTheCap()
        cappedMarksKeepTheirShare()
        aMonthIsThirtyDays()
        onlyConfidentSendersAreOnTheDeck()
        readCloselyOnTopKeepsASenderOffTheDeck()
        noLinkAndAlreadyMutedIsNothingToOffer()
        theDeckKeepsTheDaemonsOrder()
        theFixtureShowsOnlyTheSendersWorthAsking()
        aFewCandidatesAreNeededBeforeItIsOffered()
        aLinkMeansUnsubscribeAndNoLinkMeansBlock()
        aRuleNeedsASentenceAndAnUnruledSender()
        decidingStagesAndAdvances()
        aChoiceNotOnOfferIsRefused()
        aRuleSurvivesBackAndConfirm()
        finishingEarlyLeavesTheRestUntouched()
        progressIsAPositionThatStopsAtTheTotal()
        theStagedSummaryAddsUp()
        onlyKeepsChangesNothing()
        perMonthIsRoundedOnce()
        aStatusIsWhatTheJobSaysNow()
        theLiveSummaryCountsOnlyWhatHappened()
        thePlanNamesEverySenderUnderWhatHappensToIt()
        theLineUnderTheChoicesSaysOnlyWhatAButtonCannot()
        aTypedRuleIsTrimmedCappedAndNeverEmpty()
        aTypedRuleSurvivesBackAndConfirm()
        leavingParksWhatIsStagedAndEndsWhatIsNot()
        onlyAParkedSessionExpiresAndTouchingResetsIt()
        commitLeavesAloneWhatWasEndedSince()
        theChartsAreDrawnFromTheDaemonsNumbers()
        theRingsPartsAlwaysMakeTheWhole()
        theReadingBarSaysWhatAPersonWould()
        aParkedPurgeSaysNothingWasSent()
        theStripStopsAtItsEndsAndFoldsWhenSmall()
        theFixtureHasEveryKindOfBody()
        aSuggestionIsNeverBuiltIntoASentence()
        needsYouLinesSayWhy()
        theHeadlineFollowsWhereThingsStand()
        countsReadAsSentences()
        noDeckCopyCarriesADash()
        try smartDoneRowsDecodeLeniently()
        print("purge: ok")
    }

    /// The smart done answer's `rows`: optional, and tolerant row by row.
    static func smartDoneRowsDecodeLeniently() throws {
        func decode(_ json: String) throws -> SmartDoneResult {
            try JSONDecoder().decode(SmartDoneResult.self, from: Data(json.utf8))
        }
        let old = try decode(#"{"batch_id":"b","cleared":2,"senders":1,"by_reason":{},"message_ids":[1,2],"model":"ok"}"#)
        precondition(old.rows == nil, "a daemon without rows: no list, never a faked one")
        let got = try decode(#"""
            {"batch_id":"b","cleared":3,"senders":2,"by_reason":{"offer_ended":2},"message_ids":[912,913,914],
             "rows":[
              {"message_id":912,"reason":"offer_ended","sender":"news@example.com","sender_name":"Example Weekly",
               "subject":"48 hours only","received_at":"2026-09-28T03:09:44.036873+00:00"},
              {"message_id":913,"reason":"from_a_newer_daemon","sender":"a@example.com","sender_name":"",
               "subject":null,"received_at":null},
              {"message_id":"not a number","sender":"b@example.com"}
             ]}
            """#)
        precondition(got.rows?.count == 2, "a row that cannot be read costs itself, not the answer")
        precondition(got.rows?[0].received_at == "2026-09-28T03:09:44.036873+00:00", "kept as the string sent")
        precondition(got.rows?[1].reason == "from_a_newer_daemon", "an unknown reason still decodes")
        precondition(got.rows?[1].sender_name == "" && got.rows?[1].subject == nil)
        precondition(got.cleared == 3)
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

    static func marksAreOneEachUntilTheCap() {
        precondition(PurgeMarks.split(lit: 1, of: 23) == (1, 23))
        precondition(PurgeMarks.split(lit: 0, of: 24) == (0, 24))
        precondition(PurgeMarks.split(lit: 9, of: 4) == (4, 4), "never more lit than there are")
        precondition(PurgeMarks.split(lit: -2, of: 3) == (0, 3))
    }

    static func cappedMarksKeepTheirShare() {
        precondition(PurgeMarks.split(lit: 24, of: 48) == (12, 24))
        precondition(PurgeMarks.split(lit: 0, of: 41) == (0, 24))
        precondition(PurgeMarks.split(lit: 1, of: 200) == (1, 24), "one opened is never drawn as none")
        precondition(PurgeMarks.split(lit: 199, of: 200) == (23, 24), "one unopened is never drawn as none")
        precondition(PurgeMarks.split(lit: 200, of: 200) == (24, 24))
    }

    static func aMonthIsThirtyDays() {
        precondition(PurgeMarks.days(perMonth: 22.6) == (23, 30))
        precondition(PurgeMarks.days(perMonth: 90) == (30, 30))
        precondition(PurgeMarks.days(perMonth: 0.2) == (0, 30))
    }

    // MARK: who is on the deck

    static func onlyConfidentSendersAreOnTheDeck() {
        precondition(PurgeCard(sender("a@x.com", score: PurgeCard.endAt)) != nil, "at the line is in")
        precondition(PurgeCard(sender("b@x.com", score: PurgeCard.endAt - 1)) == nil, "one under is out")
        precondition(PurgeCard(sender("c@x.com", score: nil)) == nil, "no score is unsure")
        precondition(PurgeCard(sender("d@x.com", score: 99, reasons: [.notEnoughHistory])) != nil)
        precondition(PurgeCard.endAt == 70)
    }

    static func readCloselyOnTopKeepsASenderOffTheDeck() {
        let top = sender(
            "e@x.com", score: 95, reasons: [.readClosely(medianReadFraction: 0.9), .highVolume(perMonth: 30)])
        precondition(PurgeCard(top) == nil, "worth keeping, whatever volume did to the score")
        let lower = sender(
            "f@x.com", score: 95, reasons: [.highVolume(perMonth: 30), .readClosely(medianReadFraction: 0.9)])
        precondition(PurgeCard(lower) != nil, "only the TOP reason speaks for the sender")
    }

    static func noLinkAndAlreadyMutedIsNothingToOffer() {
        precondition(PurgeCard(sender("g@x.com", rule: .squelch, link: false)) == nil)
        precondition(PurgeCard(sender("h@x.com", rule: .squelch, link: true)) != nil, "a link is still a way out")
        precondition(PurgeCard(sender("i@x.com", rule: .filtered, link: false)) != nil, "a filtered sender can be blocked")
        precondition(PurgeCard(sender("j@x.com", link: false)) != nil)
    }

    static func theDeckKeepsTheDaemonsOrder() {
        let deck = PurgeDeck(senders: [
            sender("c@x.com", score: 90), sender("known@x.com", known: true),
            sender("a@x.com", score: 71), sender("low@x.com", score: 50), sender("b@x.com", score: 99),
        ])
        precondition(deck.cards.map(\.id) == ["c@x.com", "a@x.com", "b@x.com"], "filtered, not re-sorted")
        precondition(PurgeDeck(senders: [sender("k@x.com", known: true)]).isEmpty)
    }

    static func theFixtureShowsOnlyTheSendersWorthAsking() {
        let deck = PurgeDeck(senders: PurgeFixture.wanted + PurgeFixture.unwanted)
        precondition(deck.cards.map(\.id) == PurgeFixture.wanted.map(\.sender), "\(deck.cards.map(\.id))")
        precondition(PurgeFixture.unwanted.count >= 5)
        for s in PurgeFixture.unwanted { precondition(PurgeCard(s) == nil, "\(s.sender) must not be shown") }
        precondition(PurgeFixture.staged.count == deck.cards.count)
        var walked = deck
        for d in PurgeFixture.staged { precondition(walked.decide(d), "fixture decision \(d) must be on offer") }
    }

    static func aFewCandidatesAreNeededBeforeItIsOffered() {
        precondition(PurgeDeck.minimumCandidates == 3)
        let two = PurgeDeck(senders: [sender("a@x.com"), sender("b@x.com"), sender("low@x.com", score: 10)])
        precondition(!two.isWorthOffering, "senders the deck would not show do not count")
        let three = PurgeDeck(senders: [sender("a@x.com"), sender("b@x.com"), sender("c@x.com")])
        precondition(three.isWorthOffering)
        precondition(!PurgeDeck(senders: []).isWorthOffering)
    }

    // MARK: what a card offers

    static func aLinkMeansUnsubscribeAndNoLinkMeansBlock() {
        let linked = card(sender("a@x.com"))
        precondition(linked.ending == .unsubscribe)
        precondition(linked.allows(.unsubscribe) && !linked.allows(.block))
        let bare = card(sender("b@x.com", link: false))
        precondition(bare.ending == .block)
        precondition(bare.allows(.block) && !bare.allows(.unsubscribe))
        precondition(linked.allows(.keep) && bare.allows(.keep))
        precondition(card(sender("c@x.com", auto: false)).unsubscribeIsOnTheirSite)
        precondition(!linked.unsubscribeIsOnTheirSite && !bare.unsubscribeIsOnTheirSite)
    }

    static func aRuleNeedsASentenceAndAnUnruledSender() {
        let plain = card(sender("a@x.com"))
        precondition(plain.offersRules && plain.allows(.rule("only receipts")))
        precondition(!plain.allows(.rule("")) && !plain.allows(.rule("  \n")))
        let ruled = card(sender("b@x.com", rule: .filtered))
        precondition(!ruled.offersRules && !ruled.allows(.rule("only receipts")))
    }

    // MARK: the deck

    static func threeSenders() -> PurgeDeck {
        PurgeDeck(senders: [sender("a@x.com"), sender("b@x.com", link: false), sender("c@x.com")])
    }

    static func decidingStagesAndAdvances() {
        var deck = threeSenders()
        precondition(deck.current?.id == "a@x.com" && deck.next?.id == "b@x.com")
        precondition(deck.previous == nil)
        precondition(deck.decide(.unsubscribe))
        precondition(deck.decision(for: "a@x.com") == .unsubscribe && deck.current?.id == "b@x.com")
        precondition(deck.previous?.card.id == "a@x.com" && deck.previous?.decision == .unsubscribe)
        precondition(deck.decide(.block) && deck.decide(.keep))
        precondition(deck.isFinished && deck.current == nil && deck.next == nil)
        precondition(deck.plan.map(\.decision) == [.unsubscribe, .block, .keep])
    }

    static func aChoiceNotOnOfferIsRefused() {
        var deck = threeSenders()
        precondition(!deck.decide(.block), "a sender with a link is unsubscribed from, not blocked")
        precondition(!deck.decide(.rule("")))
        precondition(deck.current?.id == "a@x.com" && deck.plan.isEmpty, "nothing staged, nothing moved")
        precondition(deck.decide(.keep))
        precondition(!deck.decide(.unsubscribe), "no link: there is nothing to unsubscribe with")
    }

    static func aRuleSurvivesBackAndConfirm() {
        var deck = threeSenders()
        let want = "only order confirmations and delivery updates"
        precondition(deck.decide(.rule(want)))
        precondition(deck.decide(.block))
        precondition(deck.decision(for: "a@x.com") == .rule(want), "the sentence rides with the decision")
        precondition(deck.back())
        precondition(deck.current?.id == "b@x.com" && deck.decision(for: "b@x.com") == nil)
        precondition(deck.decision(for: "a@x.com") == .rule(want), "back un-stages one card only")
        deck.finishEarly()
        let groups = PurgePlan.groups(deck)
        precondition(groups.map(\.kind) == [.rule])
        precondition(groups[0].entries.map(\.detail) == [want], "and reaches the confirm page whole")
        precondition(deck.back() && deck.current?.id == "b@x.com", "back from confirm returns to the card left on")
        precondition(deck.back() && deck.current?.id == "a@x.com" && deck.plan.isEmpty)
        precondition(!deck.back(), "false at the first card")
    }

    static func finishingEarlyLeavesTheRestUntouched() {
        var deck = threeSenders()
        deck.decide(.unsubscribe)
        deck.finishEarly()
        precondition(deck.isFinished && deck.plan.count == 1)
        precondition(deck.decision(for: "b@x.com") == nil && deck.decision(for: "c@x.com") == nil)
    }

    static func progressIsAPositionThatStopsAtTheTotal() {
        var deck = threeSenders()
        precondition(deck.progress == .init(position: 1, total: 3))
        deck.decide(.keep)
        deck.decide(.keep)
        deck.decide(.keep)
        precondition(deck.progress == .init(position: 3, total: 3))
    }

    // MARK: what it comes to

    static func theStagedSummaryAddsUp() {
        var deck = PurgeDeck(senders: [
            sender("a@x.com", perMonth: 10), sender("site@x.com", perMonth: 7, auto: false),
            sender("b@x.com", perMonth: 5.4, link: false), sender("c@x.com", perMonth: 30),
            sender("d@x.com", perMonth: 40), sender("untouched@x.com", perMonth: 99),
        ])
        precondition(PurgeSummary(deck: deck).isEmpty)
        deck.decide(.unsubscribe)
        deck.decide(.unsubscribe)
        deck.decide(.block)
        deck.decide(.rule("only receipts"))
        deck.decide(.keep)
        let s = PurgeSummary(deck: deck)
        precondition(s.unsubscribed == 1 && s.needsYou == 1 && s.blocked == 1 && s.rules == 1 && s.kept == 1)
        precondition(s.ending == 2, "a site-bound unsubscribe is still one the person chose")
        precondition(s.fewerPerMonth == 15, "10 + 5.4: not the site one, the rule or the keep. got \(s.fewerPerMonth)")
        precondition(!s.isEmpty && !s.changesNothing && s.isSettled)
    }

    static func onlyKeepsChangesNothing() {
        var deck = threeSenders()
        deck.decide(.keep)
        let s = PurgeSummary(deck: deck)
        precondition(!s.isEmpty && s.changesNothing && s.kept == 1 && s.fewerPerMonth == 0)
        var ruled = threeSenders()
        ruled.decide(.rule("only receipts"))
        precondition(!PurgeSummary(deck: ruled).changesNothing, "a rule alone changes what arrives")
    }

    static func perMonthIsRoundedOnce() {
        var deck = PurgeDeck(senders: (0..<5).map { sender("s\($0)@x.com", perMonth: 0.4) })
        for _ in 0..<5 { deck.decide(.unsubscribe) }
        precondition(PurgeSummary(deck: deck).fewerPerMonth == 2, "five at 0.4 are two, not zero")
    }

    static func aStatusIsWhatTheJobSaysNow() {
        precondition(PurgeStatus.resolve(.unsubscribing, job: nil, opened: false) == .running, "accepted, no row yet")
        precondition(PurgeStatus.resolve(.unsubscribing, job: .running, opened: false) == .running)
        precondition(PurgeStatus.resolve(.unsubscribing, job: .done, opened: false) == .unsubscribed)
        precondition(
            PurgeStatus.resolve(.unsubscribing, job: .needsYou(reason: "choose_topics"), opened: false)
                == .needsYou(reason: "choose_topics"))
        precondition(
            PurgeStatus.resolve(.unsubscribing, job: .needsYou(reason: nil), opened: true) == .opened)
        precondition(
            PurgeStatus.resolve(.onTheirSite, job: nil, opened: false)
                == .needsYou(reason: PurgeStatus.siteOnly))
        precondition(PurgeStatus.resolve(.onTheirSite, job: .done, opened: true) == .opened)
        precondition(PurgeStatus.resolve(.blocked, job: nil, opened: false) == .blocked)
        precondition(PurgeStatus.resolve(.ruled("x"), job: nil, opened: false) == .ruled("x"))
        precondition(PurgeStatus.resolve(.kept, job: nil, opened: false) == .kept)
        precondition(PurgeStatus.resolve(.failed, job: .done, opened: false) == .failed)
    }

    static func theLiveSummaryCountsOnlyWhatHappened() {
        let cards = (0..<8).map { card(sender("s\($0)@x.com", perMonth: 10)) }
        let statuses: [PurgeStatus] = [
            .running, .unsubscribed, .needsYou(reason: nil), .opened, .blocked, .ruled("x"), .kept,
            .failed,
        ]
        let s = PurgeSummary(statuses: zip(cards, statuses).map { (card: $0, status: $1) })
        precondition(s.running == 1 && s.unsubscribed == 1 && s.needsYou == 1 && s.opened == 1)
        precondition(s.blocked == 1 && s.rules == 1 && s.kept == 1 && s.failed == 1)
        precondition(s.fewerPerMonth == 20, "the one that went through and the block, nothing else")
        precondition(!s.isSettled)
        let done = PurgeSummary(statuses: [(card: cards[0], status: .unsubscribed)])
        precondition(done.isSettled && done.fewerPerMonth == 10)
    }

    // MARK: the confirm page

    static func thePlanNamesEverySenderUnderWhatHappensToIt() {
        var deck = PurgeDeck(senders: [
            sender("keep@x.com"), sender("a@x.com"), sender("b@x.com", link: false),
            sender("site@x.com", auto: false), sender("r@x.com"), sender("c@x.com"),
            sender("untouched@x.com"),
        ])
        deck.decide(.keep)
        deck.decide(.unsubscribe)
        deck.decide(.block)
        deck.decide(.unsubscribe)
        deck.decide(.rule("just the invoices"))
        deck.decide(.unsubscribe)
        deck.finishEarly()
        let groups = PurgePlan.groups(deck)
        precondition(groups.map(\.kind) == [.unsubscribe, .block, .rule, .keep], "in the order they matter")
        precondition(groups[0].entries.map(\.id) == ["a@x.com", "site@x.com", "c@x.com"], "deck order within")
        precondition(groups[0].entries.map(\.detail) == [nil, PurgeCopy.siteDetail, nil])
        precondition(groups[1].entries.map(\.id) == ["b@x.com"] && groups[1].entries[0].detail == nil)
        precondition(groups[2].entries.map(\.detail) == ["just the invoices"])
        precondition(groups[3].entries.map(\.id) == ["keep@x.com"])
        precondition(!groups.flatMap(\.entries).contains { $0.id == "untouched@x.com" })
        precondition(PurgePlan.groups(PurgeDeck(senders: [sender("z@x.com")])).isEmpty, "empty groups are left out")
    }

    // MARK: copy

    static func theLineUnderTheChoicesSaysOnlyWhatAButtonCannot() {
        let linked = card(sender("a@x.com"))
        let bare = card(sender("b@x.com", link: false))
        let site = card(sender("s@x.com", auto: false))
        precondition(PurgeCopy.title(bare.ending) == "Block" && PurgeCopy.title(linked.ending) == "Unsubscribe")
        precondition(PurgeCopy.title(.rule("x")) == "Create rule" && PurgeCopy.title(.ownRule("x")) == "Create rule")
        precondition(PurgeCopy.title(.keep) == "Keep", "keeping, not skipping")
        // Nothing pointed at: empty, EXCEPT the two a person must not find out afterwards.
        precondition(PurgeCopy.note(nil, on: linked) == "")
        precondition(PurgeCopy.note(nil, on: bare) == "No unsubscribe link in any of their emails.")
        precondition(PurgeCopy.note(nil, on: site) == "You finish on their site, at the end.")
        precondition(PurgeCopy.note(.ending, on: linked) == "Asks them to stop.")
        precondition(PurgeCopy.note(.ending, on: bare) == "No unsubscribe link in any of their emails.")
        precondition(PurgeCopy.note(.ending, on: site) == "You finish on their site, at the end.")
        precondition(PurgeCopy.note(.rule, on: bare) == "Keep part of what they send.")
        precondition(PurgeCopy.note(.keep, on: site) == "Leaves them as they are.")
        precondition(PurgeCopy.note(.block) == "No unsubscribe link in any of their emails.")
    }

    static func aSuggestionIsNeverBuiltIntoASentence() {
        // In the owner's voice and any grammatical shape: nothing here may
        // assume it starts with "only".
        for want in ["only order confirmations", "mute the marketing", "just the invoices"] {
            precondition(PurgeCopy.staged(.rule(want), name: "Ox") == "Will add a rule for Ox: \(want)")
            precondition(PurgeCopy.status(.ruled(want)) == "Rule: \(want)")
        }
        precondition(PurgeCopy.staged(.ownRule("mute the marketing"), name: "Ox") == "Will add a rule for Ox: mute the marketing")
        precondition(!PurgeCopy.rulesNote.lowercased().contains("only show") && PurgeCopy.rulesNote.hasSuffix("."))
    }

    static func needsYouLinesSayWhy() {
        precondition(PurgeCopy.needsYou(PurgeStatus.siteOnly) == "Their unsubscribe only works on their site.")
        precondition(PurgeCopy.needsYou("choose_topics") == "Their page asks which emails to stop.")
        precondition(PurgeCopy.needsYou("needs_browser") == "Their page only works in a browser.")
        precondition(PurgeCopy.needsYou("link_error") == "Their link didn't work from here.")
        precondition(PurgeCopy.needsYou(nil) == "We couldn't confirm it went through.")
        precondition(PurgeCopy.needsYou("from_the_future") == "We couldn't confirm it went through.")
        precondition(PurgeCopy.status(.needsYou(reason: "choose_topics")) == "Their page asks which emails to stop.")
        precondition(PurgeCopy.status(.running) == "Unsubscribing" && PurgeCopy.status(.unsubscribed) == "Unsubscribed")
    }

    static func theHeadlineFollowsWhereThingsStand() {
        var s = PurgeSummary()
        s.running = 1
        s.unsubscribed = 2
        s.fewerPerMonth = 40
        precondition(PurgeCopy.headline(s) == "Working through them.", "not a claim while jobs run")
        s.running = 0
        s.needsYou = 2
        precondition(PurgeCopy.headline(s) == "2 need you to finish.", "hands before good news")
        s.needsYou = 1
        precondition(PurgeCopy.headline(s) == "1 needs you to finish.")
        s.needsYou = 0
        precondition(PurgeCopy.headline(s) == "About 40 fewer emails a month.")
        s.fewerPerMonth = 1
        precondition(PurgeCopy.headline(s) == "About 1 fewer email a month.")
        s.fewerPerMonth = 0
        s.rules = 2
        precondition(PurgeCopy.headline(s) == "2 new rules.")
        s.rules = 1
        precondition(PurgeCopy.headline(s) == "1 new rule.")
        s.rules = 0
        precondition(PurgeCopy.headline(s) == "All done.")
    }

    static func countsReadAsSentences() {
        precondition(PurgeCopy.failedNote(0) == nil)
        precondition(PurgeCopy.failedNote(1) == "1 did not go through. Nothing changed for that sender.")
        precondition(PurgeCopy.failedNote(3) == "3 did not go through. Nothing changed for those senders.")
        precondition(PurgeCopy.fewer(0) == nil)
        precondition(PurgeCopy.fewer(1) == "about 1 fewer email a month")
        precondition(PurgeCopy.fewer(60) == "about 60 fewer emails a month")
        precondition(PurgeCopy.keptOnly(1) == "Kept 1 sender." && PurgeCopy.keptOnly(4) == "Kept 4 senders.")
    }

    static func noDeckCopyCarriesADash() {
        let cards = [
            card(sender("a@x.com")), card(sender("b@x.com", auto: false)),
            card(sender("c@x.com", link: false)),
        ]
        var all: [String] = [
            PurgeCopy.rulesNote, PurgeCopy.rulesLoading, PurgeCopy.otherTitle, PurgeCopy.otherPrompt,
            PurgeLeave.applyingToast, PurgeCopy.resumePill, PurgeCopy.parked(1), PurgeCopy.parked(3),
            PurgeCopy.discardAsk(1), PurgeCopy.discardAsk(2), PurgeCopy.expired,
            PurgeCopy.siteDetail, PurgeCopy.needsYouTitle, PurgeCopy.needsYouLead, PurgeCopy.laterNote,
            PurgeCopy.tooFew, PurgeCopy.keptOnly(1), PurgeCopy.keptOnly(2),
            PurgeCopy.failedNote(1) ?? "", PurgeCopy.failedNote(2) ?? "",
            PurgeCopy.fewer(1) ?? "", PurgeCopy.fewer(2) ?? "",
        ]
        let decisions: [PurgeDecision] = [.unsubscribe, .block, .rule("x"), .ownRule("x"), .keep]
        for c in cards {
            for choice: PurgeCopy.Choice? in [nil, .ending, .rule, .keep] { all.append(PurgeCopy.note(choice, on: c)) }
            let e = PurgeEvidence(c.sender)
            all += [e.spoken(ledgerDays: 34), e.readLabel ?? "", e.readCaption ?? "", e.volumeLabel ?? ""]
        }
        for d in decisions { all += [PurgeCopy.title(d), PurgeCopy.staged(d, name: "x")] }
        for k in PurgeDecision.Kind.allCases { all += [PurgeCopy.heading(k), PurgeCopy.note(k) ?? ""] }
        let statuses: [PurgeStatus] = [
            .running, .unsubscribed, .needsYou(reason: nil), .needsYou(reason: PurgeStatus.siteOnly),
            .needsYou(reason: "choose_topics"), .needsYou(reason: "needs_browser"),
            .needsYou(reason: "link_error"), .opened, .blocked, .ruled("x"), .kept, .alreadyDone,
            .failed,
        ]
        for s in statuses { all.append(PurgeCopy.status(s)) }
        var s = PurgeSummary()
        for mutate: (inout PurgeSummary) -> Void in [
            { $0.running = 1 }, { $0.running = 0; $0.needsYou = 2 }, { $0.needsYou = 0; $0.fewerPerMonth = 9 },
            { $0.fewerPerMonth = 0; $0.rules = 2 }, { $0.rules = 0 },
        ] {
            mutate(&s)
            all.append(PurgeCopy.headline(s))
        }
        for line in all {
            precondition(!line.contains("\u{2014}") && !line.contains("\u{2013}"), line)
            precondition(!line.contains(" - "), line)
        }
    }

    // MARK: a rule in the person's own words

    static func aTypedRuleIsTrimmedCappedAndNeverEmpty() {
        precondition(PurgeRuleText.accept("  just the invoices \n") == "just the invoices")
        precondition(PurgeRuleText.accept("") == nil && PurgeRuleText.accept("   \n\t ") == nil, "empty is refused")
        precondition(PurgeRuleText.accept("only\nreceipts") == "only receipts", "a one-line field")
        let long = String(repeating: "a", count: PurgeRuleText.limit + 50)
        precondition(PurgeRuleText.accept(long)?.count == PurgeRuleText.limit, "cut at the limit")
        precondition(PurgeRuleText.accept(String(repeating: "a", count: PurgeRuleText.limit))?.count == PurgeRuleText.limit)
        precondition(PurgeRuleText.limit == 300)
        let c = card(sender("a@x.com"))
        precondition(c.allows(.ownRule("just the invoices")))
        precondition(!c.allows(.ownRule("")) && !c.allows(.ownRule(" padded ")), "only what accept hands back")
        precondition(!c.allows(.ownRule(long)))
        precondition(!card(sender("r@x.com", rule: .filtered)).allows(.ownRule("just the invoices")))
        precondition(PurgeDecision.ownRule("x").kind == .rule && PurgeDecision.ownRule("x").want == "x")
        precondition(PurgeDecision.rule("y").want == "y" && PurgeDecision.keep.want == nil)
        precondition(PurgeDecision.ownRule("x") != PurgeDecision.rule("x"), "typed and suggested are sent differently")
    }

    static func aTypedRuleSurvivesBackAndConfirm() {
        var deck = threeSenders()
        precondition(deck.decide(.ownRule("just the invoices")))
        precondition(deck.decide(.block))
        precondition(deck.back())
        precondition(deck.decision(for: "a@x.com") == .ownRule("just the invoices"), "still typed, still whole")
        deck.decide(.block)
        deck.decide(.rule("only receipts"))
        let groups = PurgePlan.groups(deck)
        precondition(groups.map(\.kind) == [.block, .rule])
        precondition(
            groups[1].entries.map(\.detail) == ["just the invoices", "only receipts"],
            "a typed rule is listed exactly as a suggested one is")
        precondition(PurgeSummary(deck: deck).rules == 2)
    }

    // MARK: leaving, parking, expiring

    static func leavingParksWhatIsStagedAndEndsWhatIsNot() {
        precondition(PurgeLeave.rule(stage: .deciding, anythingStaged: false) == .end)
        precondition(PurgeLeave.rule(stage: .deciding, anythingStaged: true) == .park)
        precondition(PurgeLeave.rule(stage: .confirming, anythingStaged: true) == .park)
        precondition(PurgeLeave.rule(stage: .confirming, anythingStaged: false) == .end)
        precondition(PurgeLeave.rule(stage: .applying, anythingStaged: true) == .stay, "the requests are out")
        precondition(PurgeLeave.rule(stage: .applying, anythingStaged: false) == .stay)
    }

    static func onlyAParkedSessionExpiresAndTouchingResetsIt() {
        let t0 = Date(timeIntervalSince1970: 2_000_000)
        func at(_ minutes: Double) -> Date { t0.addingTimeInterval(minutes * 60) }
        precondition(PurgeClock.parkedLifetime == 30 * 60)
        var clock = PurgeClock(at: t0)
        precondition(!clock.isExpired(at: at(29), parked: true), "29 minutes parked survives")
        precondition(clock.isExpired(at: at(31), parked: true), "31 minutes parked expires")
        precondition(clock.isExpired(at: at(30), parked: true), "at the line it is over")
        precondition(!clock.isExpired(at: at(500), parked: false), "one on screen never times out")
        precondition(clock.isExpired(at: at(8 * 60), parked: true), "a night asleep counts: wall clock")
        clock.touch(at: at(25))
        precondition(clock.touchedAt == at(25))
        precondition(!clock.isExpired(at: at(31), parked: true), "a decision at 25 resets it")
        precondition(!clock.isExpired(at: at(54), parked: true) && clock.isExpired(at: at(55), parked: true))
        precondition(!clock.isExpired(at: at(-90), parked: true), "a clock set backwards expires nothing")
    }

    static func commitLeavesAloneWhatWasEndedSince() {
        let ending: [PurgeDecision] = [.unsubscribe, .block, .rule("x"), .ownRule("x")]
        for d in ending {
            precondition(PurgeReconcile.action(for: d, asked: false, blocked: false, wasBlocked: false) == .apply)
            precondition(
                PurgeReconcile.action(for: d, asked: true, blocked: false, wasBlocked: false) == .alreadyDone,
                "unsubscribed from by another route: not asked twice")
            precondition(
                PurgeReconcile.action(for: d, asked: false, blocked: true, wasBlocked: false) == .alreadyDone,
                "blocked since: a rule now would replace that block")
            precondition(
                PurgeReconcile.action(for: d, asked: false, blocked: true, wasBlocked: true) == .apply,
                "muted BEFORE the deck was built is why it is on the deck, not news")
            precondition(PurgeReconcile.action(for: d, asked: true, blocked: true, wasBlocked: true) == .alreadyDone)
        }
        for asked in [true, false] {
            for blocked in [true, false] {
                precondition(
                    PurgeReconcile.action(for: .keep, asked: asked, blocked: blocked, wasBlocked: false) == .apply,
                    "a keep is only a note")
            }
        }
        precondition(PurgeStatus.resolve(.alreadyDone, job: .running, opened: false) == .alreadyDone)
        let s = PurgeSummary(statuses: [(card: card(sender("a@x.com", perMonth: 10)), status: .alreadyDone)])
        precondition(s.alreadyDone == 1 && s.unsubscribed == 0 && s.fewerPerMonth == 0, "claimed as nothing")
        precondition(!s.isEmpty && s.isSettled)
        precondition(PurgeCopy.status(.alreadyDone) == "Already taken care of")
    }

    // MARK: the numbers the charts draw

    static func evidence(
        received: Int, opened: Int, cleared: Int, readSample: Int = 0, fraction: Double? = nil,
        words: Int? = nil, dwellSample: Int = 0, dwellMs: Int? = nil, perMonth: Double? = 4,
        reasons: [PurgeReason] = []
    ) -> PurgeEvidence {
        PurgeEvidence(
            PurgeSender(
                sender: "a@x.com", display_name: "A", open_count: 1, open_message_ids: [1],
                signals: .init(
                    received: received, opened: opened, cleared_unopened: cleared,
                    dwell_sample: dwellSample, median_dwell_ms: dwellMs, clicks: 0,
                    read_sample: readSample, median_read_fraction: fraction, median_words: words,
                    per_month: perMonth), score: 90, reasons: reasons))
    }

    static func theChartsAreDrawnFromTheDaemonsNumbers() {
        let e = evidence(
            received: 23, opened: 1, cleared: 14, readSample: 1, fraction: 0.08, words: 2400,
            dwellSample: 1, dwellMs: 6000, perMonth: 22.6, reasons: [.neverClicked(opened: 1)])
        precondition(e.received == 23 && e.opened == 1 && e.clearedUnopened == 14 && e.untouched == 8)
        precondition(e.hasOpenRate && e.neverClicked)
        precondition(e.readFraction == 0.08 && e.perMonth == 22.6)
        precondition(e.volumeLabel == "About 23 a month")
        precondition(
            e.spoken(ledgerDays: 34)
                == "Opened 1 of 23. cleared 14 without opening. 8 left unopened. Read about 8% of each. about 2,400 words long, open for 6 seconds. About 23 a month. Never clicked a link. in the last 34 days",
            e.spoken(ledgerDays: 34))
        // Nothing the daemon did not send is drawn.
        let bare = evidence(received: 0, opened: 0, cleared: 0, fraction: 0.5, dwellMs: 4000, perMonth: nil)
        precondition(!bare.hasOpenRate && bare.readFraction == nil, "a fraction with no sample is not a measurement")
        precondition(bare.dwellMs == nil && bare.perMonth == nil && bare.volumeLabel == nil && bare.readLabel == nil)
        precondition(!bare.neverClicked)
        precondition(bare.spoken(ledgerDays: 1) == "in the last day")
        // per_month absent on the wire falls back to the reason that carries it.
        precondition(
            evidence(received: 3, opened: 0, cleared: 0, perMonth: nil, reasons: [.highVolume(perMonth: 31.2)]).perMonth == 31.2)
        precondition(evidence(received: 3, opened: 0, cleared: 0, perMonth: 5, reasons: [.highVolume(perMonth: 31.2)]).perMonth == 5)
        precondition(evidence(received: 41, opened: 0, cleared: 12).spoken(ledgerDays: 34).hasPrefix("Opened none of 41."))
    }

    static func theRingsPartsAlwaysMakeTheWhole() {
        for (r, o, c) in [(23, 1, 14), (10, 10, 0), (10, 0, 10), (5, 9, 9), (5, 3, 9), (0, 4, 4), (7, -2, -3)] {
            let e = evidence(received: r, opened: o, cleared: c)
            precondition(e.opened >= 0 && e.clearedUnopened >= 0 && e.untouched >= 0, "\((r, o, c))")
            precondition(e.opened + e.clearedUnopened + e.untouched == e.received, "\((r, o, c))")
            let s = e.shares
            if e.received > 0 { precondition(abs(s.opened + s.cleared + s.untouched - 1) < 1e-9) }
        }
        let over = evidence(received: 5, opened: 3, cleared: 9)
        precondition(over.opened == 3 && over.clearedUnopened == 2 && over.untouched == 0, "cleared cannot overfill")
        let e = evidence(received: 20, opened: 5, cleared: 10)
        precondition(e.shares.opened == 0.25 && e.shares.cleared == 0.5 && e.shares.untouched == 0.25)
        let none = evidence(received: 0, opened: 0, cleared: 0).shares
        precondition(none.opened == 0 && none.cleared == 0 && none.untouched == 0)
    }

    static func theReadingBarSaysWhatAPersonWould() {
        func label(_ f: Double) -> String? { evidence(received: 3, opened: 1, cleared: 0, readSample: 1, fraction: f).readLabel }
        precondition(label(0.08) == "Read about 8% of each")
        precondition(label(0.004) == "Read almost none of each")
        precondition(label(0.012) == "Read about 1% of each")
        precondition(label(0.996) == "Read about 99% of each", "never 100 short of the end")
        precondition(label(1) == "Read each to the end" && label(1.4) == "Read each to the end")
        precondition(evidence(received: 3, opened: 1, cleared: 0, readSample: 1, fraction: 1.4).readFraction == 1)
        precondition(evidence(received: 3, opened: 1, cleared: 0, readSample: 1, fraction: -0.2).readFraction == 0)
        precondition(evidence(received: 3, opened: 1, cleared: 0, words: 800).readCaption == "about 800 words long")
        precondition(
            evidence(received: 3, opened: 1, cleared: 0, dwellSample: 2, dwellMs: 4000).readCaption == "open for 4 seconds")
        precondition(evidence(received: 3, opened: 1, cleared: 0).readCaption == nil)
    }

    static func aParkedPurgeSaysNothingWasSent() {
        precondition(PurgeCopy.chosen(1) == "1 choice" && PurgeCopy.chosen(3) == "3 choices")
        precondition(PurgeCopy.parked(3) == "Purge paused with 3 choices. Nothing has been sent.")
        precondition(PurgeCopy.parked(1) == "Purge paused with 1 choice. Nothing has been sent.")
        precondition(PurgeCopy.discardAsk(3) == "Discard 3 choices?")
        precondition(PurgeCopy.expired == "Your purge timed out. Nothing was sent.")
        precondition(PurgeCopy.resumePill == "resume purge")
    }

    // MARK: the strip beside the rendered email

    static func theStripStopsAtItsEndsAndFoldsWhenSmall() {
        precondition(PurgeStrip.move(0, by: 1, count: 14) == 1)
        precondition(PurgeStrip.move(3, by: -1, count: 14) == 2)
        precondition(PurgeStrip.move(0, by: -1, count: 14) == 0, "no wrap off the top")
        precondition(PurgeStrip.move(13, by: 1, count: 14) == 13, "no wrap off the bottom into the newest")
        precondition(PurgeStrip.move(5, by: 1, count: 0) == 0)
        precondition(PurgeStrip.shown(0, count: 3) == 0 && PurgeStrip.shown(2, count: 3) == 2)
        precondition(PurgeStrip.shown(3, count: 3) == 0, "a selection off the end shows the newest")
        precondition(PurgeStrip.shown(-1, count: 3) == 0)
        precondition(PurgeStrip.shown(0, count: 0) == nil, "nothing to show for an empty strip")
        // The window's minimum is 980 wide and the rail takes 60 of it.
        precondition(PurgeStrip.isCompact(width: 920), "at the minimum window the strip is dates only")
        precondition(!PurgeStrip.isCompact(width: 1140), "at the default window it is not")
        precondition(PurgeStrip.isCompact(width: 1039.9) && !PurgeStrip.isCompact(width: 1040))
    }

    static func theFixtureHasEveryKindOfBody() {
        guard PurgeFixture.body == nil else { return }
        func kind(_ id: Int) -> String {
            switch PurgeFixture.rendered(subject: "S", messageId: id) {
            case .html: "html"
            case .text: "text"
            case .failed: "failed"
            }
        }
        precondition(kind(9001) == "html" && kind(9101) == "html", "a promo and a long newsletter")
        precondition(kind(9601) == "text", "one plain-text email")
        precondition(kind(9801) == "failed", "one that does not load")
        if case .html(let promo) = PurgeFixture.rendered(subject: "Subject line", messageId: 9001) {
            precondition(promo.contains("Subject line") && promo.contains("href="), "links, to prove they are inert")
        }
    }
}
