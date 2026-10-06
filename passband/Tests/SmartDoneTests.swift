// The smart done receipt's words. Every line is a claim about what was taken
// off somebody's board, so the plurals, the order and the accounting are
// pinned rather than read.

import Foundation

@main
struct SmartDoneTests {
    static func main() {
        theHeadlineCountsEmails()
        sendersAreNamedOnlyWhenKnown()
        everyReasonHasALineInOrder()
        aSingleOfEachReadsAsOne()
        zeroReasonsAreLeftOut()
        anUnknownReasonIsStillAccountedFor()
        nothingClearedSaysWhy()
        undoneSaysWhatCameBack()
        noLineCarriesADash()
        everyRowSaysWhy()
        theListIsGroupedByWhyInTheReceiptsOrder()
        sendersStayTogetherNewestFirst()
        theDaemonsTimestampShapeSorts()
        anEmptyNameFallsBackToTheAddress()
        aPauseMovesTheDeadlineByExactlyTheTimePaused()
        pausingTwiceKeepsTheFirstPause()
        pausingAfterExpiryDoesNothing()
        theRingHoldsStillWhilePaused()
        print("smart-done: ok")
    }

    static func theHeadlineCountsEmails() {
        precondition(SmartDoneCopy.headline(cleared: 43) == "43 emails cleared")
        precondition(SmartDoneCopy.headline(cleared: 1) == "1 email cleared")
    }

    static func sendersAreNamedOnlyWhenKnown() {
        precondition(SmartDoneCopy.senders(17) == "from 17 senders")
        precondition(SmartDoneCopy.senders(1) == "from 1 sender")
        precondition(SmartDoneCopy.senders(0) == nil)
    }

    static func everyReasonHasALineInOrder() {
        let lines = SmartDoneCopy.lines(byReason: [
            "stale_promo": 11, "ignored_sender": 20, "expired": 4, "offer_ended": 5,
            "event_passed": 3,
        ])
        precondition(
            lines.map(\.spoken) == [
                "3 events that already happened", "5 offers that ended", "4 no longer current",
                "20 from senders you never open", "11 promotions you never opened",
            ], "\(lines.map(\.spoken))")
    }

    static func aSingleOfEachReadsAsOne() {
        let lines = SmartDoneCopy.lines(byReason: [
            "stale_promo": 1, "ignored_sender": 1, "expired": 1, "offer_ended": 1,
            "event_passed": 1,
        ])
        precondition(
            lines.map(\.spoken) == [
                "1 event that already happened", "1 offer that ended", "1 no longer current",
                "1 from senders you never open", "1 promotion you never opened",
            ], "\(lines.map(\.spoken))")
    }

    static func zeroReasonsAreLeftOut() {
        let lines = SmartDoneCopy.lines(byReason: ["offer_ended": 2, "event_passed": 0])
        precondition(lines.map(\.spoken) == ["2 offers that ended"])
        precondition(SmartDoneCopy.lines(byReason: [:]).isEmpty)
    }

    static func anUnknownReasonIsStillAccountedFor() {
        let lines = SmartDoneCopy.lines(byReason: ["offer_ended": 2, "from_the_future": 3, "x": 1])
        precondition(lines.map(\.spoken) == ["2 offers that ended", "4 others"], "\(lines)")
        precondition(lines.map(\.count).reduce(0, +) == 6, "the lines add up to what was cleared")
        precondition(SmartDoneCopy.lines(byReason: ["x": 1]).map(\.spoken) == ["1 other"])
    }

    static func nothingClearedSaysWhy() {
        precondition(SmartDoneCopy.nothing(model: "ok") == "Reading is already current.")
        precondition(SmartDoneCopy.nothing(model: nil) == "Reading is already current.")
        precondition(
            SmartDoneCopy.nothing(model: "skipped_unconfigured") == "Reading is already current.")
        precondition(
            SmartDoneCopy.nothing(model: "skipped_nothing_to_ask") == "Reading is already current.")
        precondition(SmartDoneCopy.modelMissed("skipped_failed"))
        precondition(SmartDoneCopy.modelMissed("skipped_cap"))
        precondition(!SmartDoneCopy.modelMissed("ok"))
        precondition(
            SmartDoneCopy.nothing(model: "skipped_failed")
                == "Nothing obvious to clear. The closer look did not run this time.")
    }

    static func undoneSaysWhatCameBack() {
        precondition(SmartDoneCopy.undone(reopened: 43) == "43 emails are back in Reading.")
        precondition(SmartDoneCopy.undone(reopened: 1) == "1 email is back in Reading.")
    }

    static func noLineCarriesADash() {
        var all = [
            SmartDoneCopy.headline(cleared: 2), SmartDoneCopy.senders(2) ?? "",
            SmartDoneCopy.nothing(model: "ok"), SmartDoneCopy.nothing(model: "skipped_cap"),
            SmartDoneCopy.unavailable, SmartDoneCopy.failed, SmartDoneCopy.undone(reopened: 2),
        ]
        for n in [1, 2] {
            all += SmartDoneCopy.lines(
                byReason: Dictionary(
                    uniqueKeysWithValues: (SmartDoneCopy.order + ["zz"]).map { ($0, n) })
            ).map(\.spoken)
        }
        for line in all {
            precondition(!line.contains("\u{2014}") && !line.contains("\u{2013}"), line)
            precondition(!line.contains(" - "), line)
        }
    }

    // MARK: the full list

    static func item(
        _ id: Int, _ reason: String, _ sender: String, at: String? = nil, name: String? = nil
    ) -> SmartDoneItem {
        SmartDoneItem(
            id: id, reason: reason, sender: sender, senderName: name, subject: "s\(id)", receivedAt: at)
    }

    static func everyRowSaysWhy() {
        precondition(SmartDoneCopy.why("event_passed") == "The event already happened")
        precondition(SmartDoneCopy.why("offer_ended") == "The offer ended")
        precondition(SmartDoneCopy.why("expired") == "No longer current")
        precondition(SmartDoneCopy.why("ignored_sender") == "From a sender you never open")
        precondition(SmartDoneCopy.why("stale_promo") == "A promotion you never opened")
        precondition(SmartDoneCopy.why("from_the_future") == "Cleared", "neutral, never a guess")
        precondition(SmartDoneCopy.why("") == "Cleared")
        for code in SmartDoneCopy.order + ["zz"] {
            let line = SmartDoneCopy.why(code)
            precondition(!line.contains("\u{2014}") && !line.contains("\u{2013}") && !line.contains(" - "))
        }
    }

    static func theListIsGroupedByWhyInTheReceiptsOrder() {
        let groups = SmartDoneList.groups([
            item(1, "stale_promo", "a@x.com"), item(2, "from_the_future", "b@x.com"),
            item(3, "event_passed", "c@x.com"), item(4, "offer_ended", "d@x.com"),
            item(5, "offer_ended", "e@x.com"), item(6, "", "f@x.com"),
        ])
        precondition(
            groups.map(\.reason) == ["event_passed", "offer_ended", "stale_promo", SmartDoneList.other],
            "\(groups.map(\.reason))")
        precondition(
            groups.map(\.heading) == [
                "1 event that already happened", "2 offers that ended",
                "1 promotion you never opened", "2 others",
            ], "the page says what the receipt says: \(groups.map(\.heading))")
        precondition(groups.map(\.items.count).reduce(0, +) == 6, "every row is somewhere")
        precondition(
            SmartDoneList.groups([item(9, "from_the_future", "z@x.com")]).map(\.heading) == ["1 other"])
        precondition(SmartDoneList.groups([]).isEmpty)
    }

    static func sendersStayTogetherNewestFirst() {
        let rows = [
            item(1, "offer_ended", "old@x.com", at: "2026-09-01T10:00:00Z"),
            item(2, "offer_ended", "busy@x.com", at: "2026-09-10T10:00:00Z"),
            item(3, "offer_ended", "mid@x.com", at: "2026-09-20T10:00:00Z"),
            item(4, "offer_ended", "busy@x.com", at: "2026-09-28T10:00:00Z"),
            item(5, "offer_ended", "undated@x.com"),
            item(6, "offer_ended", "busy@x.com", at: "2026-09-15T10:00:00Z"),
        ]
        let got = SmartDoneList.groups(rows)[0].items.map(\.id)
        // busy's newest (the 28th) puts all three of busy's first, newest
        // first among themselves; then mid, old, and the undated one last.
        precondition(got == [4, 6, 2, 3, 1, 5], "\(got)")
        precondition(SmartDoneList.sorted(rows.reversed()).map(\.id) == got, "whatever order they came in")
    }

    static func theDaemonsTimestampShapeSorts() {
        // Exactly what the daemon stores: sub-second digits, numeric offset.
        let a = SmartDoneList.date("2026-09-28T03:09:44.036873+00:00")
        let b = SmartDoneList.date("2026-09-28T03:09:45.000001+00:00")
        precondition(a != .distantPast && b != .distantPast, "the shape must parse")
        precondition(b.timeIntervalSince(a) == 1)
        precondition(a == SmartDoneList.date("2026-09-28T03:09:44Z"))
        precondition(SmartDoneList.date("2026-09-28T05:09:44.5+02:00") == a, "offsets are honoured")
        precondition(SmartDoneList.date(nil) == .distantPast && SmartDoneList.date("soon") == .distantPast)
        let got = SmartDoneList.sorted([
            item(1, "expired", "a@x.com", at: "2026-09-27T03:09:44.036873+00:00"),
            item(2, "expired", "b@x.com", at: "2026-09-28T03:09:44.036873+00:00"),
        ]).map(\.id)
        precondition(got == [2, 1])
    }

    static func anEmptyNameFallsBackToTheAddress() {
        precondition(item(1, "expired", "a@x.com", name: "").displayName == "a@x.com")
        precondition(item(1, "expired", "a@x.com", name: "  ").displayName == "a@x.com")
        precondition(item(1, "expired", "a@x.com", name: nil).displayName == "a@x.com")
        precondition(item(1, "expired", "a@x.com", name: "Example Weekly").displayName == "Example Weekly")
    }

    // MARK: the undo clock

    static let t0 = Date(timeIntervalSince1970: 1_000_000)
    static func at(_ seconds: TimeInterval) -> Date { t0.addingTimeInterval(seconds) }

    static func aPauseMovesTheDeadlineByExactlyTheTimePaused() {
        var clock = SmartDoneClock(startedAt: t0, lasting: 30)
        precondition(clock.remaining(at: at(10)) == 20 && !clock.isPaused)
        precondition(clock.pause(at: at(10)))
        precondition(clock.remaining(at: at(500)) == 20, "no time is lost while paused")
        precondition(!clock.isExpired(at: at(500)))
        precondition(clock.resume(at: at(52.5)))
        precondition(clock.expiresAt == at(72.5), "30 + exactly the 42.5 spent paused")
        precondition(clock.remaining(at: at(52.5)) == 20, "carries on from where it stopped")
        precondition(clock.remaining(at: at(72.5)) == 0 && clock.isExpired(at: at(72.5)))
        precondition(!clock.resume(at: at(60)), "resuming a running clock does nothing")
        precondition(clock.expiresAt == at(72.5))
    }

    static func pausingTwiceKeepsTheFirstPause() {
        var clock = SmartDoneClock(startedAt: t0, lasting: 30)
        precondition(clock.pause(at: at(5)))
        precondition(!clock.pause(at: at(9)), "already stopped")
        precondition(clock.pausedAt == at(5))
        clock.resume(at: at(15))
        precondition(clock.expiresAt == at(40), "moved by 10, from the FIRST pause")
        // And a second visit to the page is its own pause.
        precondition(clock.pause(at: at(20)))
        clock.resume(at: at(21))
        precondition(clock.expiresAt == at(41))
    }

    static func pausingAfterExpiryDoesNothing() {
        var clock = SmartDoneClock(startedAt: t0, lasting: 30)
        precondition(!clock.pause(at: at(30)), "at the deadline there is nothing left to protect")
        precondition(!clock.pause(at: at(31)))
        precondition(!clock.isPaused && clock.expiresAt == at(30) && clock.isExpired(at: at(31)))
        precondition(clock.remaining(at: at(31)) == 0, "a late pause must not bring the window back")
    }

    static func theRingHoldsStillWhilePaused() {
        var clock = SmartDoneClock(startedAt: t0, lasting: 30)
        precondition(clock.fraction(at: t0) == 1 && clock.fraction(at: at(15)) == 0.5)
        clock.pause(at: at(15))
        precondition(clock.fraction(at: at(900)) == 0.5)
        clock.resume(at: at(100))
        precondition(clock.fraction(at: at(100)) == 0.5, "and picks up at the same place")
        precondition(clock.fraction(at: at(107.5)) == 0.25)
        precondition(clock.fraction(at: at(999)) == 0)
    }
}
