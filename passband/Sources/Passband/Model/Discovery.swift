// The discovery ledger's shell: UserDefaults, the practice gate, analytics, and
// the one pending celebration. The rules are `DiscoveryLedger`'s; this file only
// decides where a move comes from and where its answer goes.
//
// ONE ENTRY POINT. Every call site is `Discovery.note(.move)`, one line beside
// the analytics capture it shadows, placed where the action has SUCCEEDED (the
// server said yes), never on a keypress or a palette opening. A rule editor
// that was opened and closed is not somebody who found rules.
//
// NOT ROUTED THROUGH ANALYTICS. Analytics gates itself on the telemetry level
// and on release builds, and the ring has to fill for somebody who turned
// telemetry off. So the ledger records first, and the `feature_found` /
// `feature_kept` events are a side effect that Analytics may drop.
//
// PER INSTALL, like everything in Prefs' charter: one JSON blob under
// `passband.discovery.ledger`. Two Macs fill two rings, which is the honest
// answer to "what has this person found on this machine".
//
// FLOW'S TWO MOVES live in AppStore, beside the walk they describe:
//
//   * `.walkDeep` when a walk's `done + skipped` reaches 3 (`walk`'s didSet):
//     far enough in that it was a walk and not a stray Space.
//   * `.walkFinished` in `AppStore.finishWalk()`: the walk run to its end.

import Foundation
import Observation

@MainActor
@Observable
final class Discovery {
    static let shared = Discovery()

    private static let ledgerKey = "passband.discovery.ledger"

    private(set) var ledger: DiscoveryLedger

    /// A set found and waiting for its moment. The VIEW layer decides when the
    /// app is at rest (no sheet, no flight, no tour) and calls
    /// `celebrationShown()`; a find mid-action is held here rather than
    /// interrupting the thing that earned it.
    private(set) var pendingCelebration: FeatureSet?

    /// False for a launch under `PASSBAND_DISCOVERY`: the forced states are a
    /// look, and writing them back would make somebody's real ledger a lie.
    @ObservationIgnored private let persists: Bool
    @ObservationIgnored private let defaults = UserDefaults.standard

    private init() {
        let env = ProcessInfo.processInfo.environment
        if let spec = env["PASSBAND_DISCOVERY"] {
            ledger = DiscoveryLedger.forcing(
                DiscoveryLedger.parseOverride(spec),
                fresh: DiscoveryLedger.parseFreshOverride(spec), now: Date())
            persists = false
        } else {
            ledger = DiscoveryLedger.decoded(from: defaults.data(forKey: Self.ledgerKey))
            persists = true
            // BACKFILL. Somebody who finished the tour before this ledger
            // existed has found the basics; they did not find them just now,
            // so no event, no glow, no confetti.
            if Prefs.shared.tourCompleted, ledger.state(of: .clear) == .unseen {
                let now = Date()
                ledger.backfill(.tourCompleted, day: DiscoveryLedger.dayKey(now), now: now)
                save()
            }
        }
        // Debug only, never persisted: put a celebration on screen without
        // earning one. Same posture as `PASSBAND_FORCE_TOUR`.
        if let raw = env["PASSBAND_DISCOVERY_CELEBRATE"], let set = FeatureSet(rawValue: raw) {
            pendingCelebration = set
        }
    }

    /// The one entry point every call site uses.
    static func note(_ move: Move) { shared.record(move) }

    func state(of set: FeatureSet) -> SetState { ledger.state(of: set) }

    /// The page was visited: nothing glows any more.
    func acknowledge() {
        guard !ledger.fresh.isEmpty else { return }
        ledger.acknowledge()
        save()
    }

    /// The celebration made it on screen. Stamped HERE, not when the set was
    /// found: a find that never got its moment has not used up the day's one.
    func celebrationShown() {
        pendingCelebration = nil
        ledger.lastCelebratedDay = DiscoveryLedger.dayKey(Date())
        save()
    }

    private func record(_ move: Move) {
        guard DiscoveryLedger.counts(move, practice: RehearsalMode.isEnabled) else { return }
        let now = Date()
        let today = DiscoveryLedger.dayKey(now)
        let event = ledger.note(move, day: today, now: now)
        save()
        switch event {
        case .found(let set):
            Analytics.capture("feature_found", ["set": set.rawValue])
            // One a day, and never over one already waiting: the second find
            // still lights its slice and still glows, it just does not get a
            // moment of its own.
            if pendingCelebration == nil,
                DiscoveryLedger.mayCelebrate(
                    lastCelebratedDay: ledger.lastCelebratedDay, today: today)
            {
                pendingCelebration = set
            }
        case .kept(let set):
            // Kept is quiet on purpose: it happens on an ordinary third day,
            // mid-task, and the ring changing is the whole announcement.
            Analytics.capture("feature_kept", ["set": set.rawValue])
        case nil:
            break
        }
    }

    private func save() {
        guard persists, let data = ledger.encoded() else { return }
        defaults.set(data, forKey: Self.ledgerKey)
    }
}
