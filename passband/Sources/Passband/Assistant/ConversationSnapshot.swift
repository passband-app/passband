// ONE CONVERSATION, AT REST: what a question in search history keeps so that
// tapping it opens the chat the reader already had, follow-ups and all, rather
// than paying for (and waiting on) the same digging again.
//
// Pure Foundation over the types in Transcript.swift, so test.sh can round-trip
// one. The session takes and restores these (`AssistantSession.snapshot()` /
// `restore(_:)`); QuestionChats keeps them on disk.
//
// TAKEN BETWEEN RUNS ONLY. A live run owns its history, its parked cards and
// its continuations, none of which can be written down. What CAN be left
// half-finished in the transcript of a run that ended is tidied by `sanitized`:
// a saved card must never come back looking as if it is still waiting on a
// tap, because there is no run behind it to answer one.

import Foundation

struct ConversationSnapshot: Codable, Sendable {
    /// Bumped if this shape ever changes incompatibly. A file written under
    /// another version is simply not restored: the question is asked again.
    static let currentVersion = 1

    var version: Int = Self.currentVersion
    /// What the tray draws, oldest first.
    var transcript: [ChatItem]
    /// The provider's view, so a follow-up after restoring is a real follow-up.
    var history: [Wire.MessageParam]
    /// The pin each question was asked under; the switch note derives from it.
    var askedThreadIds: [String?]
    /// The session's id counter, so rows added after a restore never collide
    /// with restored ones.
    var nextItemId: Int

    /// Nothing worth keeping: no question was ever answered or even asked.
    var isEmpty: Bool { transcript.isEmpty && history.isEmpty }

    /// The snapshot as it may be written down. Three things a finished run can
    /// leave mid-flight on screen, each settled to what it now really is:
    ///   * a confirm card still `.pending` or `.running` has nobody behind it
    ///     any more, so it reads as declined;
    ///   * a tool chip still spinning did not finish, so it reads as failed;
    ///   * an assistant bubble that never got a word is dropped from the end.
    static func sanitized(
        transcript: [ChatItem], history: [Wire.MessageParam], askedThreadIds: [String?],
        nextItemId: Int
    ) -> ConversationSnapshot {
        var items = transcript.map { item -> ChatItem in
            var item = item
            if let state = item.action?.state, state == .pending || state == .running {
                item.action?.state = .declined
            }
            if item.tool?.state == .running { item.tool?.state = .failed }
            return item
        }
        while let last = items.last, last.kind == .assistant,
            last.text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        {
            items.removeLast()
        }
        return ConversationSnapshot(
            transcript: items, history: history, askedThreadIds: askedThreadIds,
            nextItemId: nextItemId)
    }

    // MARK: - codec

    private static let encoder: JSONEncoder = {
        let e = JSONEncoder()
        // Stable bytes for the same conversation, so a re-save of an unchanged
        // chat is recognisably unchanged.
        e.outputFormatting = [.sortedKeys]
        return e
    }()

    func encoded() throws -> Data { try Self.encoder.encode(self) }

    /// nil for anything that does not read back cleanly, including a version
    /// this build does not know: the caller's answer to every one of those is
    /// the same, which is to ask the question again.
    static func decode(_ data: Data) -> ConversationSnapshot? {
        guard let snapshot = try? JSONDecoder().decode(ConversationSnapshot.self, from: data),
            snapshot.version == currentVersion
        else { return nil }
        return snapshot
    }
}
