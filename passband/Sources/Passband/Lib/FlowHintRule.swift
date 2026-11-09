// WHO GETS FLOW MODE'S ONE-TIME EXPLANATION, the first time a walk starts on
// this Mac. Pure, so test.sh pins it; the shell is Model/FlowHint.swift.
//
// Anybody who has walked before already knows what flow mode is, whatever
// this Mac's flag says: the discovery ledger decides that (a deep walk or a
// finished one counted, or the flow set found or kept), so an install that
// predates the flag never sees it.

import Foundation

enum FlowHintRule {
    static func everWalked(walkDeep: Int, walkFinished: Int, flow: SetState) -> Bool {
        walkDeep > 0 || walkFinished > 0 || flow != .unseen
    }

    static func owed(seen: Bool, everWalked: Bool) -> Bool {
        !seen && !everWalked
    }
}
