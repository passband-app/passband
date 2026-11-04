// THE ONE-TIME "STAYS HERE" NOTE, the shell around StaysHintRule. The reader's
// esc tells this which For your eyes email was just left (`leaving`); when
// the board comes back, that row asks whether to light up (`rowReturned`),
// with whether it is actually in view. It lights for a few seconds and goes.
// Drawn by Views/StaysHintMark.swift; takes no clicks and binds no keys.
//
// Mac only in practice: the phone has no esc.
//
// `PASSBAND_STAYS_HINT=1` forces it on the first esc of the launch out of a
// For your eyes email, and saves nothing. The override lives here.

import Foundation
import Observation

@MainActor
@Observable
final class StaysHint {
    static let shared = StaysHint()
    static let forced = ProcessInfo.processInfo.environment["PASSBAND_STAYS_HINT"] == "1"

    /// The row lit right now, by thread id.
    private(set) var shownFor: String?

    /// Long enough to read twice.
    static let hold: Duration = .seconds(4.5)

    @ObservationIgnored private(set) var pendingFor: String?
    @ObservationIgnored private var pendingExpiry: Task<Void, Never>?
    @ObservationIgnored private var holdTask: Task<Void, Never>?
    @ObservationIgnored private var forcedSeen = false

    private var seen: Bool {
        get { Self.forced ? forcedSeen : Prefs.shared.staysHintSeen }
        set { if Self.forced { forcedSeen = newValue } else { Prefs.shared.staysHintSeen = newValue } }
    }

    private var everDone: Bool {
        if Self.forced { return false }
        return ReaderHintRule.everDone(
            doneCount: Discovery.shared.ledger.moveCounts[.emailDone] ?? 0,
            clear: Discovery.shared.state(of: .clear))
    }

    /// Esc in the reader, just before it closes.
    func leaving(_ store: AppStore) {
        guard Self.forced || !FixtureMode.isEnabled else { return }
        guard let id = store.threadId, store.activeView == .sitrep, store.walk == nil else { return }
        let fye = store.sitrep.standing.contains { $0.thread_id == id }
        guard StaysHintRule.owed(seen: seen, everDone: everDone, inForYourEyes: fye) else { return }
        pendingFor = id
        // The row answers in the same frame the reader closes. One that never
        // does (it left the band) must not light up on some later close.
        pendingExpiry?.cancel()
        pendingExpiry = Task { @MainActor in
            try? await Task.sleep(for: .seconds(1))
            guard !Task.isCancelled else { return }
            self.pendingFor = nil
        }
    }

    /// The board is back and this row is on it.
    func rowReturned(_ id: String, visible: Bool) {
        guard pendingFor == id else { return }
        pendingFor = nil
        pendingExpiry?.cancel()
        // Out of view: still owed, for the next first esc.
        guard StaysHintRule.shows(owedOnLeave: true, rowVisible: visible) else { return }
        seen = true
        if !Self.forced { Analytics.capture("stays_hint_shown") }
        shownFor = id
        holdTask?.cancel()
        holdTask = Task { @MainActor in
            try? await Task.sleep(for: Self.hold)
            guard !Task.isCancelled else { return }
            self.shownFor = nil
        }
    }
}
