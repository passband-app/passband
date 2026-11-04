// WHO GETS THE ONE-TIME "STAYS HERE" NOTE: the first time on this Mac that
// somebody leaves a For your eyes email with esc without marking it done,
// the row they left is ringed on the board with "Stays here until you mark
// it done." Pure, so test.sh pins it; the shell is Model/StaysHint.swift.
//
// Only For your eyes counts (Reading, receipts and the rest are not a to-do
// list), and never for anybody who has marked an email done: they know.

import Foundation

enum StaysHintRule {
    static func owed(seen: Bool, everDone: Bool, inForYourEyes: Bool) -> Bool {
        !seen && !everDone && inForYourEyes
    }

    /// Shown only where it can be seen: a row scrolled away, or gone from the
    /// band, leaves the note owed for the next first esc.
    static func shows(owedOnLeave: Bool, rowVisible: Bool) -> Bool {
        owedOnLeave && rowVisible
    }
}
