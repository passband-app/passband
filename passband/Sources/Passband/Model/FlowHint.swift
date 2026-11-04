// FLOW MODE'S ONE-TIME EXPLANATION, the shell around FlowHintRule. The first
// time a walk starts on this Mac, a card says what flow mode is over the
// email for two seconds, then shrinks into flow mode's own corner hint
// (`WalkHint`, in ThreadViewer.swift) and hands over to it. Drawn by
// Views/FlowHintLayer.swift; never takes focus and binds no keys.
//
// It never shares the moment with the reader hint's centred beat: a walk that
// opens the very first email waits for that one to reach its corner first.
//
// `PASSBAND_FLOW_HINT=1` forces it on the first walk of the launch and saves
// nothing. The override lives here, nowhere else.

import Foundation
import Observation

@MainActor
@Observable
final class FlowHint {
    static let shared = FlowHint()
    static let forced = ProcessInfo.processInfo.environment["PASSBAND_FLOW_HINT"] == "1"

    enum Placement: Equatable, Sendable {
        /// Over the email, explaining.
        case centred
        /// Travelling into flow mode's corner hint, drawn as it.
        case landing
    }

    /// Nil draws nothing, and lets the real corner hint show.
    private(set) var placement: Placement?

    static let beat: Duration = .seconds(2)
    /// The travel's length, after which the real corner hint takes over.
    static let travel: Duration = .milliseconds(750)

    @ObservationIgnored private var forcedSeen = false
    @ObservationIgnored private var task: Task<Void, Never>?

    private var seen: Bool {
        get { Self.forced ? forcedSeen : Prefs.shared.flowHintSeen }
        set { if Self.forced { forcedSeen = newValue } else { Prefs.shared.flowHintSeen = newValue } }
    }

    private var everWalked: Bool {
        if Self.forced { return false }
        let moves = Discovery.shared.ledger.moveCounts
        return FlowHintRule.everWalked(
            walkDeep: moves[.walkDeep] ?? 0, walkFinished: moves[.walkFinished] ?? 0,
            flow: Discovery.shared.state(of: .flow))
    }

    /// A walk started. `stillWalking` is asked again after any wait, so a
    /// walk that ended first shows nothing.
    func walkStarted(stillWalking: @escaping @MainActor () -> Bool) {
        guard Self.forced || !FixtureMode.isEnabled else { return }
        guard FlowHintRule.owed(seen: seen, everWalked: everWalked) else { return }
        seen = true
        task?.cancel()
        task = Task { @MainActor in
            // A tick for the reader to open (and the reader hint to decide),
            // then never over its centred beat.
            try? await Task.sleep(for: .milliseconds(150))
            while ReaderHint.shared.placement == .centred {
                try? await Task.sleep(for: .milliseconds(100))
                if Task.isCancelled { return }
            }
            guard !Task.isCancelled, stillWalking() else { return }
            if !Self.forced { Analytics.capture("flow_hint_shown") }
            self.placement = .centred
            try? await Task.sleep(for: Self.beat)
            guard !Task.isCancelled, stillWalking() else {
                self.placement = nil
                return
            }
            self.placement = .landing
            try? await Task.sleep(for: Self.travel)
            self.placement = nil
        }
    }

    /// The walk ended: take the card with it.
    func walkEnded() {
        task?.cancel()
        task = nil
        placement = nil
    }
}
