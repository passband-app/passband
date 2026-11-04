// THE FIRST LOOK AT A NEWLY CONNECTED MAILBOX.
//
// A fresh mailbox opens onto a daemon that is still walking it: the board is
// empty, then sparse, then right. Rather than open onto that, the first
// connect on this Mac veils the real board (blurred, live, filling in behind)
// under one card that says what is happening and roughly how long it has
// left (Views/FirstLoadVeil.swift). It is not a lesson and it teaches nothing:
// it goes the moment the catch-up does, or sooner if the person says so.
//
// DECIDED ONCE, AT THE FIRST STATS. A catch-up in flight shows the veil. A
// mailbox with mail and no catch-up goes straight to the board. A mailbox
// with neither (the daemon has not started walking yet) is given a short wait
// for one to appear. Every outcome stamps `Prefs.firstLoadSeen`, so a later
// account, a reconnect or a relaunch never sees it again.
//
// `PASSBAND_FIRST_LOAD=1` forces it with made-up numbers, past the stamp and
// with no daemon needed, for looking at the card. It stamps nothing.

import Foundation
import Observation

@MainActor
@Observable
final class FirstLoad {
    static let forced = ProcessInfo.processInfo.environment["PASSBAND_FIRST_LOAD"] == "1"

    /// What the card draws: the walk's progress and the board's three counts.
    struct Snapshot: Equatable, Sendable {
        var done: Int
        var total: Int
        var sorted: Int
        var noise: Int
        var attention: Int

        /// The one mapping from a walk and the stats to the card, for the
        /// veil on every poll, its full bar at the end, and the preview.
        init(done: Int, total: Int, stats: StoreStats?) {
            self.done = done
            self.total = total
            sorted = stats?.total ?? 0
            noise = stats?.tier_counts["noise"] ?? 0
            attention = stats?.bands.standing ?? 0
        }
    }

    /// The veil is up.
    private(set) var active = false
    /// The walk has ended and the bar is being shown full for a beat before
    /// the veil lifts.
    private(set) var finishing = false
    private(set) var snapshot: Snapshot?

    /// How long a mailbox with no mail and no catch-up is waited on before
    /// the board is simply shown.
    static let decisionWait: TimeInterval = 20
    /// The full bar's beat before the veil lifts.
    static let settle: Duration = .seconds(1)

    @ObservationIgnored private var decided = false
    @ObservationIgnored private var waitingSince: Date?
    /// The store epoch the decision was made under. An account change under
    /// the veil (account actions are blocked while it is up, so this is a
    /// backstop) must not finish one mailbox's walk with another's numbers.
    @ObservationIgnored private var epoch: Int?
    @ObservationIgnored private var settleTask: Task<Void, Never>?
    @ObservationIgnored private var synthetic: Task<Void, Never>?

    /// Called on connect and on every poll (RootView's ShellWatchers). Cheap
    /// and idempotent: after the decision it only feeds a veil that is up.
    func observe(_ store: AppStore, now: Date = Date()) {
        if Self.forced {
            startSynthetic(store)
            return
        }
        guard !FixtureMode.isEnabled else { return }
        if let epoch, epoch != store.epoch, !Prefs.shared.firstLoadSeen {
            // A different mailbox: start its first look over, quietly.
            settleTask?.cancel()
            settleTask = nil
            active = false
            finishing = false
            snapshot = nil
            decided = false
            waitingSince = nil
            self.epoch = nil
        }
        if !decided {
            guard !Prefs.shared.firstLoadSeen else {
                decided = true
                return
            }
            guard store.connStatus == .connected, let stats = store.stats else { return }
            epoch = store.epoch
            // A walk still under way, not one first seen already complete:
            // that would blink the card for its settle and count a wait
            // nobody had.
            if let c = store.catchUp, c.done < c.total {
                decided = true
                active = true
                Analytics.capture("first_load_shown")
            } else if stats.total > 0 || store.catchUp != nil || waited(now) {
                decided = true
                conclude(store)
                return
            } else {
                return
            }
        }
        guard active, !finishing else { return }
        if let progress = store.catchUp {
            snapshot = Snapshot(done: progress.done, total: progress.total, stats: store.stats)
            // The pace itself is the store's (`catchUpEstimate`), fed by the
            // same polls, so the board's banner carries it on after Start now.
            if progress.done < progress.total { return }
        }
        // Gone, or reporting itself complete: the walk is over.
        if store.stats != nil { finish(store) }
    }

    /// "Start now", Enter, Esc, a click on the scrim.
    func dismiss(_ store: AppStore = .shared) {
        guard active else { return }
        lift(store, early: !finishing)
    }

    private func waited(_ now: Date) -> Bool {
        guard let since = waitingSince else {
            waitingSince = now
            return false
        }
        return now.timeIntervalSince(since) >= Self.decisionWait
    }

    /// The bar to the end, a beat to see it there, then the board.
    private func finish(_ store: AppStore) {
        finishing = true
        if let shown = snapshot {
            snapshot = Snapshot(done: shown.total, total: shown.total, stats: store.stats)
        }
        settleTask = Task { @MainActor in
            try? await Task.sleep(for: Self.settle)
            guard !Task.isCancelled, self.active else { return }
            self.lift(store, early: false)
        }
    }

    private func lift(_ store: AppStore, early: Bool) {
        if !Self.forced { Analytics.capture("first_load_ended", ["early": early]) }
        settleTask?.cancel()
        settleTask = nil
        active = false
        finishing = false
        conclude(store)
    }

    /// The first look is over, whether or not the veil was ever up.
    private func conclude(_ store: AppStore) {
        guard !Self.forced else { return }
        let firstTime = !Prefs.shared.firstLoadSeen
        Prefs.shared.firstLoadSeen = true
        #if os(macOS)
            // Somebody new to the app is shown the latest release's notes on
            // their first time at the board, and nothing older.
            if firstTime { store.whatsNew.meetNewcomer() }
        #endif
    }

    // MARK: - the forced preview

    /// Made-up numbers on a clock, so the card can be looked at with no
    /// daemon: a walk of 4,200 that started a while ago, moving about twelve
    /// a second, over the connected board's real counts. It keeps counting
    /// after Start now so the board's banner can be looked at too, and ends
    /// the way a real walk does. Once per launch: a dismissed preview stays
    /// dismissed.
    ///
    /// The one hook it needs in the store is `stageSyntheticCatchUp`: the
    /// banner after Start now reads the store's sync status and pace, so the
    /// made-up walk has to be what those see.
    private func startSynthetic(_ store: AppStore) {
        guard !decided else { return }
        decided = true
        active = true
        let started = Date()
        synthetic = Task { @MainActor in
            while !Task.isCancelled {
                let elapsed = Date().timeIntervalSince(started)
                let done = min(4_200, 1_180 + Int(elapsed * 12))
                store.stageSyntheticCatchUp(CatchUpProgress(done: done, total: 4_200))
                if self.active, !self.finishing {
                    self.snapshot = Snapshot(done: done, total: 4_200, stats: store.stats)
                }
                if done >= 4_200 {
                    if self.active { self.finish(store) }
                    store.stageSyntheticCatchUp(nil)
                    return
                }
                try? await Task.sleep(for: .seconds(3))
            }
        }
    }
}
