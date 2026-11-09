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
// ON THE MAC IT ALSO OFFERS THE IPHONE APP: a second card beside the progress, since
// the person is waiting anyway. Whether, and what happens to it when the veil
// comes down, is `TestFlightNudge`'s rule; this only says when the veil went
// up, came down, or was never needed.
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
            // SORTED, not received: the daemon's `total` counts mail still
            // waiting on triage too, and triage is held for most of a walk,
            // so it would count unsorted mail. Floored: the two are read off
            // one answer but are not one atomic count.
            let waiting = max(stats?.triage?.queued ?? 0, 0) + max(stats?.triage?.leased ?? 0, 0)
            sorted = max(0, (stats?.total ?? 0) - waiting)
            noise = stats?.tier_counts["noise"] ?? 0
            attention = stats?.bands.standing ?? 0
        }
    }

    /// The veil is up.
    private(set) var active = false
    /// The walk has ended and the bar is being shown full for a beat before
    /// the veil lifts.
    private(set) var finishing = false
    /// The counts have met but the walk is still running (the daemon between
    /// its inbox and its Sent; see FirstLoadRule): the bar full, not "ready".
    private(set) var finishingUp = false
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
    /// The veil went up for this first look, so the iPhone ask had its place
    /// in it; a first look without one owes the fallback card instead.
    @ObservationIgnored private var veilWasShown = false
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
            reset()
        }
        let connected = store.connStatus == .connected
        // Only a live poll in this epoch counts; the board painted from disk
        // carries the stats the daemon gave LAST time.
        let fresh = store.stats != nil && store.liveStatsEpoch == store.epoch
        let walk = store.stats?.catch_up.flatMap { c in
            c.total > 0 ? FirstLoadRule.Walk(done: c.done, total: c.total) : nil
        }
        if !decided {
            let decision = FirstLoadRule.decide(
                seen: Prefs.shared.firstLoadSeen, connected: connected, fresh: fresh, walk: walk,
                hasMail: (store.stats?.total ?? 0) > 0,
                waited: fresh && waited(now))
            switch decision {
            case .never:
                decided = true
                return
            case .wait:
                return
            case .conclude:
                decided = true
                conclude(store)
                return
            case .show:
                decided = true
                epoch = store.epoch
                active = true
                veilWasShown = true
                #if os(macOS)
                    TestFlightNudge.shared.veilShown(iosConnected: store.iosConnected, forced: false)
                #endif
                Analytics.capture("first_load_shown")
            }
        }
        guard active, !finishing else { return }
        switch FirstLoadRule.progress(connected: connected, fresh: fresh, walk: walk) {
        case .lost:
            dropQuietly()
        case .hold:
            break
        case .running(let met):
            finishingUp = met
            if let walk {
                snapshot = Snapshot(done: min(walk.done, walk.total), total: walk.total, stats: store.stats)
            }
        case .finished:
            finish(store)
        }
    }

    /// The connection left `.connected` (RootView, which outlives the shell
    /// this is otherwise driven from). A veil left up over the Connect gate
    /// would hold account actions blocked with no way out but a relaunch.
    func connectionChanged(_ store: AppStore) {
        guard store.connStatus != .connected, active || !decided else { return }
        if active { dropQuietly() }
    }

    /// Down without counting it as seen, so it decides again on the next real
    /// connect.
    private func dropQuietly() {
        reset()
    }

    private func reset() {
        settleTask?.cancel()
        settleTask = nil
        active = false
        finishing = false
        finishingUp = false
        snapshot = nil
        decided = false
        waitingSince = nil
        epoch = nil
        veilWasShown = false
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
        finishingUp = false
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
        #if os(macOS)
            let nudge = TestFlightNudge.shared
            if !Self.forced {
                Analytics.capture(
                    "first_load_ended",
                    ["early": early, "iphone": nudge.veilAdDrawn, "copied": nudge.copiedInVeil])
            }
            // Before `conclude`, so a hand-off card is up before What's New
            // asks to show, and What's New waits its turn behind it.
            nudge.veilEnded(early: early, iosConnected: store.iosConnected)
        #else
            if !Self.forced { Analytics.capture("first_load_ended", ["early": early]) }
        #endif
        settleTask?.cancel()
        settleTask = nil
        active = false
        finishing = false
        finishingUp = false
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
            // No veil, so no iPhone ad: the corner card, shortly, instead.
            if firstTime, !veilWasShown { TestFlightNudge.shared.askAfterOnboarding(store) }
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
        #if os(macOS)
            TestFlightNudge.shared.veilShown(iosConnected: store.iosConnected, forced: true)
        #endif
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
