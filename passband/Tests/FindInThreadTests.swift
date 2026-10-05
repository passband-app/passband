// ⌘F in the reader: the cursor that walks matches across a thread's messages.
// The thread is one thing only here (each message is its own web frame or its
// own block of text), so this is where "next" can go wrong in ways no build
// notices: stepping off the end of a message, wrapping, a backwards step into
// a message nobody has counted yet, and a count that changes when a frame
// answers with what is actually drawn.

import Foundation

@main
@MainActor
struct FindInThreadTests {
    static var failures = 0
    static var checks = 0

    static func main() {
        matchesInText()
        walkingForwardCrossesMessagesAndWraps()
        walkingBackwardAsksForTheLastMatch()
        aSearchStartsAtTheMessageBeingRead()
        theOrdinalCountsTheMessagesAbove()
        aSessionCountsWhatNobodyHasMounted()
        aFrameAnswerOutranksTheText()
        aPromisedMatchThatIsNotDrawnMovesOn()
        aFrameFindsWhatTheTextDidNot()
        aLateAnswerForAnOldTermIsDropped()
        closingForgetsTheCursorNotTheTerm()

        if failures > 0 {
            print("FAILED: \(failures) of \(checks) checks")
            exit(1)
        }
        print("ok: \(checks) checks passed")
    }

    // MARK: - TextFind

    static func matchesInText() {
        equal(TextFind.count(of: "the", in: "The cat, the hat, THE end"), 3, "case folded")
        equal(TextFind.count(of: "aa", in: "aaaa"), 2, "non-overlapping")
        equal(TextFind.count(of: "", in: "anything"), 0, "an empty term matches nothing")
        equal(TextFind.count(of: "x", in: ""), 0, "nothing in nothing")
        equal(
            TextFind.count(of: "a", in: String(repeating: "a", count: 2000)), TextFind.limit,
            "bounded")
    }

    // MARK: - FindWalk

    /// Three messages: 10 holds two matches, 20 none, 30 one.
    static let order = [10, 20, 30]
    static func held(_ id: Int) -> Int { [10: 2, 20: 0, 30: 1][id] ?? 0 }

    static func walkingForwardCrossesMessagesAndWraps() {
        var cursor = FindWalk.first(in: order, from: 0, count: held)
        equal(cursor, FindCursor(message: 10, match: 0), "first")
        cursor = FindWalk.step(cursor, by: 1, in: order, from: 0, count: held)
        equal(cursor, FindCursor(message: 10, match: 1), "second in the same message")
        cursor = FindWalk.step(cursor, by: 1, in: order, from: 0, count: held)
        equal(cursor, FindCursor(message: 30, match: 0), "skips the message with none")
        cursor = FindWalk.step(cursor, by: 1, in: order, from: 0, count: held)
        equal(cursor, FindCursor(message: 10, match: 0), "wraps to the top")
    }

    static func walkingBackwardAsksForTheLastMatch() {
        let top = FindCursor(message: 10, match: 0)
        var cursor = FindWalk.step(top, by: -1, in: order, from: 0, count: held)
        equal(cursor, FindCursor(message: 30, match: FindWalk.last), "wraps to the end")
        // The frame answers that "last" was index 0; the walk carries on from it.
        cursor = FindWalk.step(
            FindCursor(message: 30, match: 0), by: -1, in: order, from: 0, count: held)
        equal(cursor, FindCursor(message: 10, match: FindWalk.last), "back over the empty one")
        // A sentinel nobody resolved still steps: it is clamped to the count.
        cursor = FindWalk.step(cursor, by: -1, in: order, from: 0, count: held)
        equal(cursor, FindCursor(message: 10, match: 0), "the sentinel is the last index")
        // One message holding one match: next and previous are both itself.
        let only = FindCursor(message: 30, match: 0)
        let alone: (Int) -> Int = { $0 == 30 ? 1 : 0 }
        equal(FindWalk.step(only, by: 1, in: order, from: 0, count: alone), only, "alone, next")
        equal(
            FindWalk.step(only, by: -1, in: order, from: 0, count: alone),
            FindCursor(message: 30, match: FindWalk.last), "alone, previous")
        equal(
            FindWalk.step(nil, by: 1, in: order, from: 0, count: { _ in 0 }), nil,
            "nothing anywhere")
        equal(FindWalk.first(in: [], from: 0, count: held), nil, "no messages")
    }

    static func aSearchStartsAtTheMessageBeingRead() {
        equal(
            FindWalk.first(in: order, from: 1, count: held), FindCursor(message: 30, match: 0),
            "from the second message, the next one down")
        equal(
            FindWalk.first(in: order, from: 2, count: held), FindCursor(message: 30, match: 0),
            "from the last message, itself")
    }

    static func theOrdinalCountsTheMessagesAbove() {
        equal(FindWalk.ordinal(FindCursor(message: 10, match: 1), in: order, count: held), 2, "2 of 3")
        equal(FindWalk.ordinal(FindCursor(message: 30, match: 0), in: order, count: held), 3, "3 of 3")
        equal(
            FindWalk.ordinal(FindCursor(message: 10, match: FindWalk.last), in: order, count: held),
            2, "an unresolved last reads as the last")
    }

    // MARK: - ReaderFind

    static func session(_ query: String, from position: Int = 0) -> ReaderFind {
        let find = ReaderFind()
        find.begin(
            messages: [
                (id: 10, text: "invoice attached, invoice due"),
                (id: 20, text: "thanks"),
                (id: 30, text: "the Invoice again"),
            ], from: position)
        find.query = query
        find.queryChanged(from: position)
        return find
    }

    static func aSessionCountsWhatNobodyHasMounted() {
        let find = session("invoice")
        equal(find.total, 3, "counted from the text alone")
        equal(find.cursor, FindCursor(message: 10, match: 0), "parked on the first")
        equal(find.ordinal, 1, "1 of 3")
        equal(find.request(for: 10), FindRequest(query: "invoice", active: 0), "the holder")
        equal(
            find.request(for: 30), FindRequest(query: "invoice", active: FindRequest.none),
            "everyone else paints without a cursor")
        find.step(1)
        find.step(1)
        equal(find.ordinal, 3, "3 of 3")
        equal(find.cursor?.message, 30, "in the last message")
    }

    static func aFrameAnswerOutranksTheText() {
        let find = session("invoice")
        // The drawn document folds its quoted history: one match, not two.
        find.report(10, query: "invoice", count: 1, active: 0)
        equal(find.total, 2, "the frame's count replaces the estimate")
        find.step(1)
        equal(find.cursor, FindCursor(message: 30, match: 0), "one step leaves a one-match message")
        // Backwards into 10 asks for its last; the frame says that is index 0.
        find.step(-1)
        equal(find.cursor, FindCursor(message: 10, match: FindWalk.last), "asked for the last")
        find.report(10, query: "invoice", count: 1, active: 0)
        equal(find.cursor, FindCursor(message: 10, match: 0), "resolved by the answer")
    }

    static func aPromisedMatchThatIsNotDrawnMovesOn() {
        let find = session("invoice")
        find.report(10, query: "invoice", count: 0)
        equal(find.cursor, FindCursor(message: 30, match: 0), "on to the next that holds one")
        find.report(30, query: "invoice", count: 0)
        equal(find.cursor, nil, "and to nothing when none do")
        equal(find.total, 0, "no matches")
    }

    static func aFrameFindsWhatTheTextDidNot() {
        let find = session("unsubscribe")
        equal(find.cursor, nil, "the text holds none")
        find.report(20, query: "unsubscribe", count: 2)
        equal(find.cursor, FindCursor(message: 20, match: 0), "the frame that found one takes the cursor")
        equal(find.total, 2, "and the count")
    }

    static func aLateAnswerForAnOldTermIsDropped() {
        let find = session("invoice")
        find.query = "thanks"
        find.queryChanged(from: 0)
        find.report(10, query: "invoice", count: 9, active: 4)
        equal(find.total, 1, "an answer to the last term changes nothing")
        equal(find.cursor, FindCursor(message: 20, match: 0), "nor the cursor")
    }

    static func closingForgetsTheCursorNotTheTerm() {
        let find = session("invoice")
        find.close()
        equal(find.open, false, "closed")
        equal(find.request(for: 10), nil, "nothing is asked to paint")
        equal(find.query, "invoice", "the term is still in the field for next time")
        find.begin(messages: [(id: 10, text: "invoice")], from: 0)
        equal(find.cursor, FindCursor(message: 10, match: 0), "reopening searches it again")
    }

    static func equal<T: Equatable>(_ got: T, _ want: T, _ what: String) {
        checks += 1
        if got != want {
            failures += 1
            print("FAIL \(what): got \(got), want \(want)")
        }
    }
}
