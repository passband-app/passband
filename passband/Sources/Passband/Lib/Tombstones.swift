// ROWS THAT LEFT THE DAEMON, and which list fetches may still carry them.
// One ledger per kind of row the store lists from a daemon table with reused
// ids: saved drafts (`AppStore.fetchDrafts`) and the outbox
// (`AppStore.refreshScheduled`). Lifted out of the store so both kinds share
// ONE copy of the rule and test.sh can hold it to it.
//
// THE PROBLEM. A list fetch that left BEFORE a row's DELETE landed may still
// list the row, and adopting that answer resurrects something the reader
// deleted (or cancelled, or sent) and watched go. The obvious fix, a set of
// gone ids kept forever, is WRONG: both tables are `INTEGER PRIMARY KEY`
// without AUTOINCREMENT, so deleting the highest id and creating another hands
// the same id straight back, and a permanent tombstone hides that brand-new
// row until relaunch.
//
// THE RULE. Every removal is stamped with a generation, and every fetch
// records the generation it left at. A fetch hides a tombstoned id only when
// the removal completed AFTER the fetch left (it may carry the old row); one
// that left at or after the removal lists whatever that id is now, which is a
// NEW row. A tombstone is pruned as soon as no fetch still in the air could
// carry the old row, which is the OLDEST in-flight fetch, not the one that
// just landed: fetches land out of order.
//
// Foundation only, no store: pure value logic.

import Foundation

struct Tombstones: Sendable, Equatable {
    /// Bumped each time a row is known to have left the daemon.
    private(set) var generation = 0
    /// Gone ids, each with the generation its removal completed at, or
    /// `Int.max` while its DELETE is still on the wire (every fetch hides it
    /// until it lands).
    private var stones: [Int: Int] = [:]
    /// The start generations of fetches still in the air, one entry per fetch.
    private var inFlight: [Int] = []

    /// A list fetch is leaving. Returns its start generation, which the caller
    /// hands back to `hides` and `endFetch`.
    mutating func beginFetch() -> Int {
        inFlight.append(generation)
        return generation
    }

    /// That fetch landed (or failed). Its tombstones may now be prunable.
    mutating func endFetch(_ start: Int) {
        if let i = inFlight.firstIndex(of: start) { inFlight.remove(at: i) }
        prune()
    }

    /// Whether a fetch that left at `start` must drop this id.
    func hides(_ id: Int, fetchedAt start: Int) -> Bool {
        guard let gone = stones[id] else { return false }
        return gone > start
    }

    /// The DELETE for this id is on the wire: hidden from every fetch until
    /// it settles one way or the other.
    mutating func leaving(_ id: Int) { stones[id] = .max }

    /// How many tombstones are held. The ledger's size, for test.sh: pruning
    /// is invisible through `hides` by design, and a ledger that never pruned
    /// would grow for the whole account session.
    var count: Int { stones.count }

    /// Whether this id's DELETE is still on the wire.
    func isLeaving(_ id: Int) -> Bool { stones[id] == .max }

    /// The row left the daemon (its DELETE came back, or a send or schedule
    /// consumed it): stamp it with a fresh generation, so fetches already in
    /// the air still drop it and later ones do not.
    mutating func gone(_ id: Int) {
        generation += 1
        stones[id] = generation
        prune()
    }

    /// The DELETE failed and the row is still on the daemon: show it again.
    mutating func revive(_ id: Int) { stones[id] = nil }

    /// An account switch. The stones go (they name the old daemon's ids); the
    /// generation and the in-flight list do NOT: an old account's fetch still
    /// in the air removes its own entry when it lands, and a rewound counter
    /// would let it prune the new account's tombstones early.
    mutating func clear() { stones = [:] }

    /// Drop every tombstone no fetch still in the air could need.
    private mutating func prune() {
        let floor = inFlight.min() ?? generation
        stones = stones.filter { $0.value > floor }
    }
}
