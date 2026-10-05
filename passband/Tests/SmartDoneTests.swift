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
}
