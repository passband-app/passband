// The one-time "Stays here until you mark it done." note (Lib/StaysHintRule
// .swift): a first esc out of a For your eyes email, by somebody who has
// never pressed done, on a row they can see.

import Foundation

@main
struct StaysHintRuleTests {
    static func main() {
        typealias R = StaysHintRule
        expect(R.owed(seen: false, everDone: false, inForYourEyes: true), "first esc out of For your eyes")
        expect(!R.owed(seen: false, everDone: false, inForYourEyes: false), "another lane never fires it")
        expect(!R.owed(seen: false, everDone: true, inForYourEyes: true), "anyone who has pressed done knows")
        expect(!R.owed(seen: true, everDone: false, inForYourEyes: true), "once per Mac")
        expect(R.shows(owedOnLeave: true, rowVisible: true), "shown on a row in view")
        expect(!R.shows(owedOnLeave: true, rowVisible: false), "skipped off screen, so still owed")
        expect(!R.shows(owedOnLeave: false, rowVisible: true), "not owed, not shown")
        print("Stays hint rule tests passed")
    }

    static func expect(_ ok: Bool, _ what: String) {
        guard ok else { fatalError(what) }
    }
}
