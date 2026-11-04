// WHEN THE FIRST-LOAD VEIL GOES UP, STAYS UP, AND COMES DOWN. Pure, so test.sh
// pins it; the shell is Model/FirstLoad.swift, which only feeds it.
//
// CATCH-UP PRESENT MEANS RUNNING, whatever its numbers say. The daemon opens a
// walk with `catchup_begin(inbox)`, counts `done` up to it, and only THEN lists
// Sent and calls `catchup_begin_extend(inbox + sent)` (squelch-core/src/sync/
// mod.rs, the first run and the re-walk alike); `catchup_end` clears the pair
// on finish and on bail. So between the two, /stats carries done == total for
// a walk that is still going. Finished is catch_up ABSENT. Do not decide on
// `done < total`: it ends the veil early, and on a first poll in that window
// it stamps a new user as having had their first look.
//
// AND ONLY ON A FRESH POLL. The board paints from disk before the daemon has
// answered; a walk that finished while the app was closed would otherwise
// raise the veil on old numbers and blink it down at the first poll.

import Foundation

enum FirstLoadRule {
    struct Walk: Equatable, Sendable {
        var done: Int
        var total: Int
    }

    enum Decision: Equatable, Sendable {
        /// This Mac has had its first look: never.
        case never
        /// Nothing to go on yet: ask again at the next poll.
        case wait
        /// A walk is running: the veil goes up.
        case show
        /// Mail and no walk (or a long enough wait): straight to the board.
        case conclude
    }

    /// `fresh`: the stats came from a live poll in this epoch, not from disk.
    static func decide(
        seen: Bool, connected: Bool, fresh: Bool, walk: Walk?, hasMail: Bool, waited: Bool
    ) -> Decision {
        if seen { return .never }
        guard connected, fresh else { return .wait }
        if walk != nil { return .show }
        if hasMail || waited { return .conclude }
        return .wait
    }

    enum Progress: Equatable, Sendable {
        /// The connection went: drop the veil without counting it as seen, so
        /// it decides again on the next real connect.
        case lost
        /// Nothing new from a live poll: leave everything as it is.
        case hold
        /// Still going. `finishingUp` when the counts have met (the daemon
        /// between its inbox and its Sent): the bar full, but not "ready".
        case running(finishingUp: Bool)
        /// The walk is over.
        case finished
    }

    static func progress(connected: Bool, fresh: Bool, walk: Walk?) -> Progress {
        guard connected else { return .lost }
        guard fresh else { return .hold }
        guard let walk else { return .finished }
        return .running(finishingUp: walk.done >= walk.total)
    }
}
