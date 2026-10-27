// The smart clear receipt's words. Every line is a claim about what was taken
// off somebody's board, so the plurals, the order and the accounting are
// pinned rather than read.

import Foundation

@main
struct SmartClearTests {
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
        print("smart-clear: ok")
    }

    static func theHeadlineCountsEmails() {
        precondition(SmartClearCopy.headline(cleared: 43) == "43 emails cleared")
        precondition(SmartClearCopy.headline(cleared: 1) == "1 email cleared")
    }

    static func sendersAreNamedOnlyWhenKnown() {
        precondition(SmartClearCopy.senders(17) == "from 17 senders")
        precondition(SmartClearCopy.senders(1) == "from 1 sender")
        precondition(SmartClearCopy.senders(0) == nil)
    }

    static func everyReasonHasALineInOrder() {
        let lines = SmartClearCopy.lines(byReason: [
            "stale_promo": 11, "ignored_sender": 20, "expired": 4, "offer_ended": 5,
            "event_passed": 3,
        ])
        precondition(
            lines.map(\.spoken) == [
                "3 events that already happened", "5 offers that ended", "4 no longer current",
                "20 from senders you never open", "11 promotions over a week old",
            ], "\(lines.map(\.spoken))")
    }

    static func aSingleOfEachReadsAsOne() {
        let lines = SmartClearCopy.lines(byReason: [
            "stale_promo": 1, "ignored_sender": 1, "expired": 1, "offer_ended": 1,
            "event_passed": 1,
        ])
        precondition(
            lines.map(\.spoken) == [
                "1 event that already happened", "1 offer that ended", "1 no longer current",
                "1 from senders you never open", "1 promotion over a week old",
            ], "\(lines.map(\.spoken))")
    }

    static func zeroReasonsAreLeftOut() {
        let lines = SmartClearCopy.lines(byReason: ["offer_ended": 2, "event_passed": 0])
        precondition(lines.map(\.spoken) == ["2 offers that ended"])
        precondition(SmartClearCopy.lines(byReason: [:]).isEmpty)
    }

    static func anUnknownReasonIsStillAccountedFor() {
        let lines = SmartClearCopy.lines(byReason: ["offer_ended": 2, "from_the_future": 3, "x": 1])
        precondition(lines.map(\.spoken) == ["2 offers that ended", "4 others"], "\(lines)")
        precondition(lines.map(\.count).reduce(0, +) == 6, "the lines add up to what was cleared")
        precondition(SmartClearCopy.lines(byReason: ["x": 1]).map(\.spoken) == ["1 other"])
    }

    static func nothingClearedSaysWhy() {
        precondition(SmartClearCopy.nothing(model: "ok") == "Reading is already current.")
        precondition(SmartClearCopy.nothing(model: nil) == "Reading is already current.")
        precondition(
            SmartClearCopy.nothing(model: "skipped_unconfigured") == "Reading is already current.")
        precondition(
            SmartClearCopy.nothing(model: "skipped_nothing_to_ask") == "Reading is already current.")
        precondition(SmartClearCopy.modelMissed("skipped_failed"))
        precondition(SmartClearCopy.modelMissed("skipped_cap"))
        precondition(!SmartClearCopy.modelMissed("ok"))
        precondition(
            SmartClearCopy.nothing(model: "skipped_failed")
                == "Nothing obvious to clear. The closer look did not run this time.")
        precondition(SmartClearCopy.modelNote("ok", detail: "timeout") == nil)
        precondition(
            SmartClearCopy.modelNote("skipped_failed", detail: "timeout")
                == "The closer look ran out of time.")
        precondition(
            SmartClearCopy.modelNote("skipped_failed", detail: "discarded")
                == "The closer look's answer did not add up, so it was set aside.")
        precondition(
            SmartClearCopy.modelNote("skipped_failed", detail: "from_the_future")
                == SmartClearCopy.modelMissedNote)
        precondition(
            SmartClearCopy.modelNote("skipped_cap") == "The closer look is done for today.")
        precondition(SmartClearCopy.why("skimmed_sender") == "From a sender you only glance at")
        precondition(
            SmartClearCopy.lines(byReason: ["skimmed_sender": 4]).map(\.spoken)
                == ["4 from senders you only glance at"])
    }

    static func undoneSaysWhatCameBack() {
        precondition(SmartClearCopy.undone(reopened: 43) == "43 emails are back in Reading.")
        precondition(SmartClearCopy.undone(reopened: 1) == "1 email is back in Reading.")
    }

    static func noLineCarriesADash() {
        var all = [
            SmartClearCopy.headline(cleared: 2), SmartClearCopy.senders(2) ?? "",
            SmartClearCopy.nothing(model: "ok"), SmartClearCopy.nothing(model: "skipped_cap"),
            SmartClearCopy.unavailable, SmartClearCopy.failed, SmartClearCopy.undone(reopened: 2),
        ]
        for n in [1, 2] {
            all += SmartClearCopy.lines(
                byReason: Dictionary(
                    uniqueKeysWithValues: (SmartClearCopy.order + ["zz"]).map { ($0, n) })
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
    ) -> SmartClearItem {
        SmartClearItem(
            id: id, reason: reason, sender: sender, senderName: name, subject: "s\(id)", receivedAt: at)
    }

    static func everyRowSaysWhy() {
        precondition(SmartClearCopy.why("event_passed") == "The event already happened")
        precondition(SmartClearCopy.why("offer_ended") == "The offer ended")
        precondition(SmartClearCopy.why("expired") == "No longer current")
        precondition(SmartClearCopy.why("ignored_sender") == "From a sender you never open")
        precondition(SmartClearCopy.why("stale_promo") == "A promotion over a week old")
        precondition(SmartClearCopy.why("from_the_future") == "Cleared", "neutral, never a guess")
        precondition(SmartClearCopy.why("") == "Cleared")
        for code in SmartClearCopy.order + ["zz"] {
            let line = SmartClearCopy.why(code)
            precondition(!line.contains("\u{2014}") && !line.contains("\u{2013}") && !line.contains(" - "))
        }
    }

    static func theListIsGroupedByWhyInTheReceiptsOrder() {
        let groups = SmartClearList.groups([
            item(1, "stale_promo", "a@x.com"), item(2, "from_the_future", "b@x.com"),
            item(3, "event_passed", "c@x.com"), item(4, "offer_ended", "d@x.com"),
            item(5, "offer_ended", "e@x.com"), item(6, "", "f@x.com"),
        ])
        precondition(
            groups.map(\.reason) == ["event_passed", "offer_ended", "stale_promo", SmartClearList.other],
            "\(groups.map(\.reason))")
        precondition(
            groups.map(\.heading) == [
                "1 event that already happened", "2 offers that ended",
                "1 promotion over a week old", "2 others",
            ], "the page says what the receipt says: \(groups.map(\.heading))")
        precondition(groups.map(\.items.count).reduce(0, +) == 6, "every row is somewhere")
        precondition(
            SmartClearList.groups([item(9, "from_the_future", "z@x.com")]).map(\.heading) == ["1 other"])
        precondition(SmartClearList.groups([]).isEmpty)
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
        let got = SmartClearList.groups(rows)[0].items.map(\.id)
        // busy's newest (the 28th) puts all three of busy's first, newest
        // first among themselves; then mid, old, and the undated one last.
        precondition(got == [4, 6, 2, 3, 1, 5], "\(got)")
        precondition(SmartClearList.sorted(rows.reversed()).map(\.id) == got, "whatever order they came in")
    }

    static func theDaemonsTimestampShapeSorts() {
        // Exactly what the daemon stores: sub-second digits, numeric offset.
        let a = SmartClearList.date("2026-09-28T03:09:44.036873+00:00")
        let b = SmartClearList.date("2026-09-28T03:09:45.000001+00:00")
        precondition(a != .distantPast && b != .distantPast, "the shape must parse")
        precondition(b.timeIntervalSince(a) == 1)
        precondition(a == SmartClearList.date("2026-09-28T03:09:44Z"))
        precondition(SmartClearList.date("2026-09-28T05:09:44.5+02:00") == a, "offsets are honoured")
        precondition(SmartClearList.date(nil) == .distantPast && SmartClearList.date("soon") == .distantPast)
        let got = SmartClearList.sorted([
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
        var clock = SmartClearClock(startedAt: t0, lasting: 30)
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
        var clock = SmartClearClock(startedAt: t0, lasting: 30)
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
        var clock = SmartClearClock(startedAt: t0, lasting: 30)
        precondition(!clock.pause(at: at(30)), "at the deadline there is nothing left to protect")
        precondition(!clock.pause(at: at(31)))
        precondition(!clock.isPaused && clock.expiresAt == at(30) && clock.isExpired(at: at(31)))
        precondition(clock.remaining(at: at(31)) == 0, "a late pause must not bring the window back")
    }

    static func theRingHoldsStillWhilePaused() {
        var clock = SmartClearClock(startedAt: t0, lasting: 30)
        precondition(clock.fraction(at: t0) == 1 && clock.fraction(at: at(15)) == 0.5)
        clock.pause(at: at(15))
        precondition(clock.fraction(at: at(900)) == 0.5)
        clock.resume(at: at(100))
        precondition(clock.fraction(at: at(100)) == 0.5, "and picks up at the same place")
        precondition(clock.fraction(at: at(107.5)) == 0.25)
        precondition(clock.fraction(at: at(999)) == 0)
    }
}
