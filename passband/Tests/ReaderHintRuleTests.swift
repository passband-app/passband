// The reader's one-time hint (Lib/ReaderHintRule.swift): only somebody who
// has never opened the reader and never marked anything done gets the centred
// beat, the corner chip follows them until their first done, and nobody who
// has ever pressed done sees any of it.

import Foundation

@main
struct ReaderHintRuleTests {
    static func main() {
        typealias R = ReaderHintRule
        // A brand new person, first open: the centred beat, remembered.
        expect(R.onOpen(stored: nil, everDone: false) == (.centred, .seen), "first open ever is centred")
        // Later opens before a done: the corner, straight away.
        expect(R.onOpen(stored: .seen, everDone: false) == (.corner, .seen), "later opens go to the corner")
        // A done, anywhere: retired, and it stays retired.
        expect(R.onOpen(stored: .seen, everDone: true) == (.none, .retired), "a done retires it")
        expect(R.onOpen(stored: .retired, everDone: false) == (.none, .retired), "retired stays retired")
        // An existing user with no flag yet: the ledger says they know.
        expect(R.onOpen(stored: nil, everDone: true) == (.none, .retired), "an existing user never sees it")

        expect(!R.everDone(doneCount: 0, clear: .unseen), "nothing done, nothing found")
        expect(R.everDone(doneCount: 1, clear: .unseen), "one done counts")
        expect(R.everDone(doneCount: 0, clear: .found), "the basics found counts")
        expect(R.everDone(doneCount: 0, clear: .kept), "the basics kept counts")
        print("Reader hint rule tests passed")
    }

    static func expect(_ ok: Bool, _ what: String) {
        guard ok else { fatalError(what) }
    }
}
