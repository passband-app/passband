// A SENDER WHO KEPT WRITING AFTER BEING ASKED TO STOP, and the three answers
// to that: block them, smart block them, or keep them.
//
// This is the state and the verbs; Views/UnsubscribeViolationNotice.swift is
// the card. It lived as `@State` on the Mac's ActionLayer until the phone
// needed the same notice, and a second copy of "which senders have I already
// answered for" is exactly the kind of state that must not exist twice: both
// shells mount the one card off this one model.

import Foundation

@MainActor
@Observable
final class UnsubscribeViolations {
    static let shared = UnsubscribeViolations()

    /// The notice on screen. One at a time: a stack of these is a nag, not a
    /// prompt.
    private(set) var prompt: UnsubscribeRecord?
    /// An answer is in flight; the buttons hold still until it lands.
    private(set) var busy = false

    /// Senders acted on this session, so a lagging read model can't re-nag.
    @ObservationIgnored private var acted: Set<String> = []
    /// Bumped by `reset`, so a scan that was waiting on the old account's
    /// daemon cannot raise its answer over the new one.
    @ObservationIgnored private var generation = 0

    private var store: AppStore { .shared }

    /// An account switch: the notice, and the memory of what was answered,
    /// both belong to the mailbox that just left.
    func reset() {
        generation &+= 1
        prompt = nil
        busy = false
        acted = []
    }

    func scan() async {
        let gen = generation
        guard let rows = try? await APIClient.shared.getUnsubscribes(), gen == generation
        else { return }
        // Surface one at a time: a stack of these is a nag, not a prompt.
        var next = rows.first {
            $0.violation_count > 0 && $0.resolution == nil && !acted.contains($0.sender)
        }
        if var row = next {
            if row.sender == prompt?.sender {
                // A notice already up keeps what it was raised with.
                row.smart_block = row.smart_block ?? prompt?.smart_block
            } else if row.smart_block_asked != true {
                // The notice waits for its smart block, so the third button is
                // there when it arrives and not a second later. Asked once:
                // the daemon remembers the answer and sends it with the row
                // from then on. Silence on every failure (no model, an older
                // daemon) leaves the notice as the two buttons it always was.
                row.smart_block = try? await APIClient.shared.unsubSmartBlock(sender: row.sender)
                // The wait is long enough for the owner to have answered an
                // earlier notice for this sender, or to have switched accounts.
                if gen != generation || acted.contains(row.sender) { return }
            }
            next = row
        }
        // Assignment is invalidation, and the answer is nil nearly always.
        if next != prompt { prompt = next }
    }

    func block(_ record: UnsubscribeRecord) async {
        await resolve(record, failure: "block failed") { [store] in
            // Exact sender address, not the domain.
            try await Actions.createBlockRule(sender: record.sender)
            // No message on screen to name, but the rule is still a verdict on
            // the sender, so the same rows clear here as anywhere else.
            store.noteSenderResolved(record.sender)
            try await APIClient.shared.setUnsubResolution(
                sender: record.sender, resolution: .blocked)
            store.pushToast("blocked \(record.sender)", .success)
        }
    }

    func smartBlock(_ record: UnsubscribeRecord, _ smart: SmartBlockProposal) async {
        await resolve(record, failure: "smart block failed") { [store] in
            // Exact sender address, like the block. Forward-only: nothing on
            // the board moves, so there is no sender to note as resolved.
            try await Actions.createSmartBlockRule(sender: record.sender, want: smart.rule)
            try await APIClient.shared.setUnsubResolution(
                sender: record.sender, resolution: .filtered)
            store.pushToast("filtering \(record.sender), keeping \(smart.keeps)", .success)
        }
    }

    func keep(_ record: UnsubscribeRecord) async {
        // Best-effort, so no failure toast: the session-level `acted` set
        // already suppresses the re-nag.
        await resolve(record, failure: nil) {
            try await APIClient.shared.setUnsubResolution(
                sender: record.sender, resolution: .dismissed)
        }
    }

    /// The frame every answer to the notice shares: one at a time, and the
    /// notice comes down whether or not the work behind it succeeded.
    private func resolve(
        _ record: UnsubscribeRecord, failure: String?, _ work: () async throws -> Void
    ) async {
        guard !busy else { return }
        busy = true
        // Mark acted immediately so a refresh mid-flight can't re-surface it.
        acted.insert(record.sender)
        do {
            try await work()
        } catch {
            if let failure { store.pushToast(errText(error, failure), .error) }
        }
        busy = false
        prompt = nil
    }

    #if DEBUG
        /// A notice with no daemon behind it, for looking at the card.
        func stage(_ record: UnsubscribeRecord) { prompt = record }
    #endif
}
