// THE READER'S ONE-TIME HINT, the shell around ReaderHintRule: "Press e to
// mark done, or esc to leave." over the first email anyone ever opens in
// Passband on this Mac, for two seconds, then tucked into the reader's
// bottom right corner until the first done. Drawn by Views/ReaderHintLayer;
// it never takes focus and binds no keys, so e and esc work underneath it.
//
// `PASSBAND_READER_HINT=1` forces it for looking at: the centred beat on the
// first open of the launch, the corner after, gone at the next done. It
// reads and writes no flag.

import Foundation
import Observation

@MainActor
@Observable
final class ReaderHint {
    static let shared = ReaderHint()
    static let forced = ProcessInfo.processInfo.environment["PASSBAND_READER_HINT"] == "1"

    enum Placement: Equatable, Sendable { case centred, corner }

    /// Where the hint is drawn while a reader is open. Nil draws nothing.
    private(set) var placement: Placement?

    /// How long the centred beat holds before it moves to the corner.
    static let beat: Duration = .seconds(2)

    @ObservationIgnored private var forcedStored: ReaderHintRule.Stored?
    @ObservationIgnored private var baselineDone: Int?
    @ObservationIgnored private var beatTask: Task<Void, Never>?

    private var stored: ReaderHintRule.Stored? {
        get { Self.forced ? forcedStored : Prefs.shared.readerHint }
        set {
            if Self.forced { forcedStored = newValue } else { Prefs.shared.readerHint = newValue }
        }
    }

    private var doneCount: Int { Discovery.shared.ledger.moveCounts[.emailDone] ?? 0 }

    /// Whether this person has ever marked an email done. Forced, only a done
    /// since launch counts, so the preview works on a Mac that has done plenty.
    private var everDone: Bool {
        if Self.forced { return doneCount > (baselineDone ?? doneCount) }
        return ReaderHintRule.everDone(doneCount: doneCount, clear: Discovery.shared.state(of: .clear))
    }

    /// A reader opened from closed.
    func readerOpened() {
        guard Self.forced || !FixtureMode.isEnabled else { return }
        if baselineDone == nil { baselineDone = doneCount }
        guard stored != .retired else { return }
        let (show, next) = ReaderHintRule.onOpen(stored: stored, everDone: everDone)
        stored = next
        switch show {
        case .centred:
            placement = .centred
            if !Self.forced { Analytics.capture("reader_hint_shown") }
            beatTask?.cancel()
            beatTask = Task { @MainActor in
                try? await Task.sleep(for: Self.beat)
                guard !Task.isCancelled, self.placement == .centred else { return }
                self.placement = .corner
            }
        case .corner:
            placement = .corner
        case .none:
            placement = nil
        }
    }

    /// The done count moved. The first done anywhere retires the hint.
    func doneCountChanged() {
        guard stored == .seen, everDone else { return }
        stored = .retired
        beatTask?.cancel()
        placement = nil
    }
}
