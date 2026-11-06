// IS THE READER DONE TYPING? The gate between a settled query the classifier
// liked and an agent run that costs money (docs/SEARCH.md §5).
//
// The keyword search settles 220ms after the last keystroke, which is right for
// hits and far too eager for an agent: "when did i download" settled, started a
// lane, and every word after it arrived as a "refinement". So a start (or a
// refinement) waits for a longer quiet while a small model answers one question
// alongside it: does this read as finished, or cut off mid-thought? Only
// "finished" spends.

import Foundation

enum QueryReadiness {
    /// How long the field must stay unchanged, on top of the keyword debounce,
    /// before the agent starts. The judge runs during this wait, not after it.
    static let quiet = Duration.milliseconds(600)

    /// The judge's own deadline. Past it the rule's verdict stands: a slow
    /// check must not be the reason a question never gets answered.
    static let deadline = Duration.milliseconds(1200)

    static let system = """
        You judge whether someone has finished typing a search into their email app. \
        Reply with one word. Reply done if it reads as a complete question or request \
        they would want answered now. Reply typing if it is cut off mid-thought: it \
        ends on a dangling word (the, a, from, to, my, about, did i), a partial word, \
        or an unfinished clause.
        """

    /// The judge's one-word answer, read leniently. Anything but "typing" is
    /// done, so a confused reply never blocks a question.
    static func finished(reply: String) -> Bool {
        !reply.lowercased().contains("typing")
    }

    /// The request body: Haiku, no tools, a handful of output tokens.
    static func body(query: String) throws -> Data {
        let object: [String: Any] = [
            "model": AssistantModel.haiku.rawValue,
            "max_tokens": 4,
            "system": system,
            "messages": [["role": "user", "content": "Search so far: \(query)"]],
            "stream": true,
        ]
        return try JSONSerialization.data(withJSONObject: object, options: [.sortedKeys])
    }

    /// True unless the model says the query is cut off. Any failure (no key,
    /// no relay, timeout, provider error) answers true: the rule already said
    /// start, and this check only exists to hold back the half-typed.
    ///
    /// Not on the Usage ledger: it is a few dozen Haiku tokens, and counting it
    /// as an "ask" would double the search count for every question.
    @MainActor
    static func looksFinished(_ query: String) async -> Bool {
        let relay = Prefs.shared.assistantTransport == .relay && AppStore.shared.relayAvailable
        guard let body = try? body(query: query) else { return true }
        // NOT A TASK GROUP. A group waits for every child before it returns,
        // cancelled or not, so a relay stream that ignores cancellation held
        // the "deadline" open forever and the lane never started. Whichever of
        // the judge and the deadline lands first resumes this; the other is
        // cancelled and nobody waits on it.
        let verdict = Verdict()
        return await withCheckedContinuation { continuation in
            verdict.continuation = continuation
            verdict.judge = Task { @MainActor in
                var reply = ""
                var accumulator = AnthropicStreamAccumulator()
                do {
                    let stream = relay ? RelayProxy.stream(body: body) : LLMProxy.stream(body: body)
                    for try await payload in stream {
                        for event in accumulator.feed(payload) {
                            switch event {
                            case .textDelta(_, let text): reply += text
                            case .providerError: verdict.land(true); return
                            default: break
                            }
                        }
                    }
                } catch {
                    verdict.land(true); return
                }
                verdict.land(finished(reply: reply))
            }
            verdict.clock = Task { @MainActor in
                try? await Task.sleep(for: deadline)
                verdict.land(true)
            }
        }
    }

    /// The first answer wins, once; the loser is cancelled and left behind.
    @MainActor
    private final class Verdict {
        var continuation: CheckedContinuation<Bool, Never>?
        var judge: Task<Void, Never>?
        var clock: Task<Void, Never>?

        func land(_ finished: Bool) {
            guard let continuation else { return }
            self.continuation = nil
            judge?.cancel()
            clock?.cancel()
            continuation.resume(returning: finished)
        }
    }
}
