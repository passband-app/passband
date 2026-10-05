// The reader's dwell clock: what counts as time in front of a message, and
// what a click is filed under. Every rule here becomes a sentence the purge
// flow says to somebody about a sender ("about 4 seconds each", "never
// clicked"), so each one is pinned with the clock driven by hand.

import Foundation

@main
struct AttentionClockTests {
    typealias Report = AttentionClock.Report

    static func main() {
        focusThenBlurReportsTheStint()
        focusingAnotherMessageClosesTheFirst()
        pausedTimeIsNotCounted()
        aFocusRestartsAPausedStint()
        aPassThroughIsNotReading()
        aForgottenWindowIsCapped()
        aClickReportsWithoutDwell()
        aClickBelongsToTheMessageItWasIn()
        drainEmpties()
        aClockGoingBackwardsIsWorthNothing()
        leavingAListIsNotEngagingWithIt()
        print("attention-clock: ok")
    }

    static func focusThenBlurReportsTheStint() {
        var clock = AttentionClock()
        precondition(!clock.focus(7, now: 1_000), "a first focus closes nothing")
        precondition(clock.drain().isEmpty, "an open stint's time is not reported yet")
        precondition(clock.blur(now: 5_000), "blur must say it closed a stint")
        precondition(clock.drain() == [Report(messageId: 7, dwellMs: 4_000, linkClicks: 0)])
        precondition(!clock.blur(now: 9_000), "a second blur has nothing to close")
        precondition(clock.drain().isEmpty)
    }

    static func focusingAnotherMessageClosesTheFirst() {
        var clock = AttentionClock()
        clock.focus(1, now: 0)
        precondition(!clock.focus(1, now: 500), "refocusing the same message closes nothing")
        precondition(clock.focus(2, now: 3_000), "moving focus must close the old stint")
        precondition(clock.focused == 2)
        // The refocus at 500 did not restart 1's stint: it is owed all 3s.
        precondition(clock.drain() == [Report(messageId: 1, dwellMs: 3_000, linkClicks: 0)])
        clock.blur(now: 5_000)
        precondition(clock.drain() == [Report(messageId: 2, dwellMs: 2_000, linkClicks: 0)])
    }

    static func pausedTimeIsNotCounted() {
        var clock = AttentionClock()
        clock.focus(3, now: 0)
        clock.pause(now: 2_000)
        clock.pause(now: 2_500)  // a second inactive must not bank or move anything
        clock.resume(now: 60_000)
        clock.resume(now: 61_000)  // nor a second active restart the segment
        clock.blur(now: 63_000)
        precondition(
            clock.drain() == [Report(messageId: 3, dwellMs: 5_000, linkClicks: 0)],
            "2s before the pause + 3s after; the 58s away is not reading")

        // Closed while still paused: only what was banked.
        clock.focus(4, now: 100_000)
        clock.pause(now: 104_000)
        clock.blur(now: 900_000)
        precondition(clock.drain() == [Report(messageId: 4, dwellMs: 4_000, linkClicks: 0)])
    }

    static func aFocusRestartsAPausedStint() {
        var clock = AttentionClock()
        clock.focus(5, now: 0)
        clock.pause(now: 2_000)
        // The resume never arrived; the reader says the message is in focus.
        precondition(!clock.focus(5, now: 10_000))
        clock.blur(now: 13_000)
        precondition(clock.drain() == [Report(messageId: 5, dwellMs: 5_000, linkClicks: 0)])
    }

    static func aPassThroughIsNotReading() {
        var clock = AttentionClock()
        clock.focus(1, now: 0)
        clock.focus(2, now: 999)
        clock.blur(now: 1_999)
        precondition(
            clock.drain() == [Report(messageId: 2, dwellMs: 1_000, linkClicks: 0)],
            "999 ms is an arrow-key pass; exactly 1s counts")
    }

    static func aForgottenWindowIsCapped() {
        var clock = AttentionClock()
        clock.focus(1, now: 0)
        clock.blur(now: 3_600_000)
        precondition(clock.drain() == [Report(messageId: 1, dwellMs: 600_000, linkClicks: 0)])
        // The cap is per stint: two honest visits still add up.
        clock.focus(1, now: 0)
        clock.blur(now: 400_000)
        clock.focus(1, now: 500_000)
        clock.blur(now: 900_000)
        precondition(clock.drain() == [Report(messageId: 1, dwellMs: 800_000, linkClicks: 0)])
    }

    static func aClickReportsWithoutDwell() {
        var clock = AttentionClock()
        clock.focus(9, now: 0)
        clock.click(9)
        clock.click(9)
        // Sent at once, while the stint is still open and has reported no time.
        precondition(clock.drain() == [Report(messageId: 9, dwellMs: 0, linkClicks: 2)])
        // And a stint too short to count as reading still reports its click.
        clock.click(9)
        clock.blur(now: 400)
        precondition(clock.drain() == [Report(messageId: 9, dwellMs: 0, linkClicks: 1)])
    }

    static func aClickBelongsToTheMessageItWasIn() {
        var clock = AttentionClock()
        clock.focus(1, now: 0)
        clock.click(2)
        clock.blur(now: 2_000)
        precondition(
            clock.drain() == [
                Report(messageId: 1, dwellMs: 2_000, linkClicks: 0),
                Report(messageId: 2, dwellMs: 0, linkClicks: 1),
            ], "the click is 2's even though 1 held the focus; one report per message")
        // With nothing focused at all.
        clock.click(4)
        precondition(clock.drain() == [Report(messageId: 4, dwellMs: 0, linkClicks: 1)])
    }

    static func drainEmpties() {
        var clock = AttentionClock()
        clock.focus(1, now: 0)
        clock.click(1)
        clock.blur(now: 2_000)
        precondition(clock.drain().count == 1)
        precondition(clock.drain().isEmpty, "a report must not be sent twice")
        // And what was drained does not come back merged into the next visit.
        clock.focus(1, now: 10_000)
        clock.blur(now: 13_000)
        precondition(clock.drain() == [Report(messageId: 1, dwellMs: 3_000, linkClicks: 0)])
    }

    static func aClockGoingBackwardsIsWorthNothing() {
        var clock = AttentionClock()
        clock.focus(1, now: 50_000)
        clock.blur(now: 10_000)
        precondition(clock.drain().isEmpty, "a backwards clock must not underflow into a capped stint")
        clock.focus(2, now: 50_000)
        clock.pause(now: 40_000)
        clock.resume(now: 60_000)
        clock.blur(now: 62_000)
        precondition(clock.drain() == [Report(messageId: 2, dwellMs: 2_000, linkClicks: 0)])
    }

    static func leavingAListIsNotEngagingWithIt() {
        precondition(AttentionClock.isEngagement(link: "https://shop.example.com/sale/boots"))
        precondition(!AttentionClock.isEngagement(link: "https://example.com/Unsubscribe?u=abc"))
        precondition(!AttentionClock.isEngagement(link: "https://mail.example.com/e/opt-out/123"))
        precondition(!AttentionClock.isEngagement(link: "https://example.com/account/email-preferences"))
        // An opaque redirect says nothing either way, and counts.
        precondition(AttentionClock.isEngagement(link: "https://click.e.example.com/?qs=9f3a"))
    }
}
