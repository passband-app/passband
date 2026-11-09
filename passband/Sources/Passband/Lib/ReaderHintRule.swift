// WHO GETS THE READER'S ONE-TIME HINT, "Press e to mark done, or esc to
// leave.", and in which form. Pure, so test.sh pins it with no app behind it;
// the shell is Model/ReaderHint.swift.
//
// The hint is for somebody opening an email in Passband for the very first
// time. Anybody who has marked an email done already knows the key, whatever
// this Mac's flag says: the discovery ledger is the source of truth for that
// (a done counted, or the basics found or kept), so an install that predates
// the flag never sees it.

import Foundation

enum ReaderHintRule {
    /// What this Mac remembers. Absent until the reader first opens.
    enum Stored: String, Sendable {
        /// The centred beat has played; the corner chip is still owed.
        case seen
        /// Done has been pressed (or already had been): never again.
        case retired
    }

    enum Show: Equatable, Sendable {
        /// The first open ever: centred over the email, then into the corner.
        case centred
        /// A later open before the first done: straight to the corner.
        case corner
        case none
    }

    static func everDone(doneCount: Int, clear: SetState) -> Bool {
        doneCount > 0 || clear != .unseen
    }

    /// A reader opening: what to draw, and what to remember.
    static func onOpen(stored: Stored?, everDone: Bool) -> (show: Show, store: Stored) {
        if everDone { return (.none, .retired) }
        switch stored {
        case nil: return (.centred, .seen)
        case .seen: return (.corner, .seen)
        case .retired: return (.none, .retired)
        }
    }
}
