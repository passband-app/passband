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
    }

    /// The veil is up.
    private(set) var active = false
    /// The walk has ended and the bar is being shown full for a beat before
    /// the veil lifts.
    private(set) var finishing = false
    private(set) var snapshot: Snapshot?
    private(set) var estimate = CatchUpEstimate()

    /// How long a mailbox with no mail and no catch-up is waited on before
    /// the board is simply shown.
    static let decisionWait: TimeInterval = 20
    /// The full bar's beat before the veil lifts.
    static let settle: Duration = .seconds(1)

    @ObservationIgnored private var decided = false
    @ObservationIgnored private var waitingSince: Date?
    @ObservationIgnored private var settleTask: Task<Void, Never>?
    @ObservationIgnored private var synthetic: Task<Void, Never>?

    /// The line under the bar.
    var remainingPhrase: String {
        guard let snapshot else { return CatchUpEstimate.phrase(nil) }
        return CatchUpEstimate.phrase(estimate.remaining(total: snapshot.total))
    }

    /// Called on connect and on every poll (RootView's ShellWatchers). Cheap
    /// and idempotent: after the decision it only feeds a veil that is up.
    func observe(_ store: AppStore, now: Date = Date()) {
        if Self.forced {
            startSynthetic(store)
            return
        }
        guard !FixtureMode.isEnabled else { return }
        if !decided {
            guard !Prefs.shared.firstLoadSeen else {
                decided = true
                return
            }
            guard store.connStatus == .connected, let stats = store.stats else { return }
            if store.catchUp != nil {
                decided = true
                active = true
                Analytics.capture("first_load_shown")
            } else if stats.total > 0 || waited(now) {
                decided = true
                conclude(store)
                return
            } else {
                return
            }
        }
        guard active, !finishing else { return }
        if let progress = store.catchUp {
            snapshot = Snapshot(
                done: progress.done, total: progress.total, sorted: store.stats?.total ?? 0,
                noise: store.stats?.tier_counts["noise"] ?? 0,
                attention: store.stats?.bands.standing ?? 0)
            if progress.done < progress.total {
                estimate.record(done: progress.done, at: now)
                return
            }
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
        if var shown = snapshot {
            shown.done = shown.total
            shown.sorted = store.stats?.total ?? shown.sorted
            shown.noise = store.stats?.tier_counts["noise"] ?? shown.noise
            shown.attention = store.stats?.bands.standing ?? shown.attention
            snapshot = shown
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
        synthetic?.cancel()
        synthetic = nil
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
    /// a second, sampled the way the poller would be.
    /// Once per launch: a preview that was dismissed stays dismissed.
    private func startSynthetic(_ store: AppStore) {
        guard !decided else { return }
        decided = true
        active = true
        let started = Date()
        synthetic = Task { @MainActor in
            while !Task.isCancelled, self.active, !self.finishing {
                let elapsed = Date().timeIntervalSince(started)
                let done = min(4_200, 1_180 + Int(elapsed * 12))
                self.estimate.record(done: done, at: Date())
                self.snapshot = Snapshot(
                    done: done, total: 4_200, sorted: done * 9 / 10,
                    noise: done * 6 / 10, attention: max(1, done / 140))
                if done >= 4_200 {
                    self.finish(store)
                    return
                }
                try? await Task.sleep(for: .seconds(3))
            }
        }
    }
}
