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
        guard let snapshot = try? JSONDecoder().decode(ConversationSnapshot.self, from: data)
        else { return nil }
        return snapshot.restorable
    }

    /// The snapshot as it may be brought back: refused under a version this
    /// build does not know, and settled again by `sanitized` whatever the
    /// bytes say, so a file edited (or written by a bug) to hold a `.pending`
    /// card or a spinning chip can never draw a live Confirm or Edit button.
    /// Every read path goes through here, the file's per-entry one included.
    var restorable: ConversationSnapshot? {
        guard version == Self.currentVersion else { return nil }
        return Self.sanitized(
            transcript: transcript, history: history, askedThreadIds: askedThreadIds,
            nextItemId: nextItemId)
    }

    // MARK: - tools

    /// Every tool name the provider history has called. A follow-up after a
    /// restore replays these to the provider, so each must be one the session
    /// still offers.
    var calledTools: Set<String> {
        var names: Set<String> = []
        for message in history {
            guard case .blocks(let blocks) = message.content else { continue }
            for case .toolUse(_, let name, _) in blocks { names.insert(name) }
        }
        return names
    }

    /// Whether a session offering exactly `offered` can take this history up.
    /// A tool that has gone since (the Calendar tools with the page turned
    /// off, one renamed in a later build) means no: the question is asked
    /// again rather than replaying a call the request no longer defines.
    func onlyCalls(_ offered: Set<String>) -> Bool {
        calledTools.isSubset(of: offered)
    }
}

// MARK: - at rest, with an age

/// One chat as QuestionChats keeps it: the snapshot and when it was last
/// saved. The age is the expiry: a chat holds agent-door results (subjects,
/// senders, whole tool results) about mail that may since have been deleted,
/// sealed or re-triaged, and replays them to the provider on a follow-up, so
/// nothing is kept past `maxAge`. Older than that, the question is asked again.
struct SavedChat: Codable, Sendable {
    static let maxAge: TimeInterval = 30 * 24 * 60 * 60

    var savedAt: Date
    var snapshot: ConversationSnapshot

    func isFresh(now: Date) -> Bool {
        now.timeIntervalSince(savedAt) <= Self.maxAge
    }

    /// What a chats file holds, read back. nil when the file as a whole does
    /// not read (not a JSON object of entries): the caller must treat that as
    /// UNREADABLE, never as empty, or its next write would wipe every chat in
    /// it. One entry that does not read, is under another version, or has
    /// expired costs that entry alone. Each survivor is re-sanitized.
    static func readFile(_ data: Data, now: Date) -> [String: SavedChat]? {
        guard let file = try? JSONDecoder().decode([String: Lossy].self, from: data) else {
            return nil
        }
        return file.compactMapValues { entry in
            guard let saved = entry.saved, saved.isFresh(now: now),
                let snapshot = saved.snapshot.restorable
            else { return nil }
            return SavedChat(savedAt: saved.savedAt, snapshot: snapshot)
        }
    }

    /// One entry of a file about to be written: bytes already encoded (the
    /// chat just saved, encoded once to check its size), or a chat read from
    /// disk that the writer encodes off the main actor.
    enum Entry: Sendable {
        case bytes(Data)
        case chat(SavedChat)
    }

    /// A fresh one per call: the writer encodes off the main actor while a
    /// save may be encoding on it.
    private static var encoder: JSONEncoder {
        let e = JSONEncoder()
        e.outputFormatting = [.sortedKeys]
        return e
    }

    /// One chat's bytes, as they sit in the file.
    static func encodeEntry(_ chat: SavedChat) throws -> Data {
        try encoder.encode(chat)
    }

    /// The whole file: a JSON object of entries, keys sorted, assembled from
    /// each entry's bytes so a save re-encodes only the chat that changed.
    /// `readFile` reads it back.
    static func fileBytes(_ entries: [String: Entry]) throws -> Data {
        let encoder = Self.encoder
        var out = Data("{".utf8)
        for (i, key) in entries.keys.sorted().enumerated() {
            if i > 0 { out.append(Data(",".utf8)) }
            out.append(try encoder.encode(key))
            out.append(Data(":".utf8))
            switch entries[key]! {
            case .bytes(let data): out.append(data)
            case .chat(let chat): out.append(try encoder.encode(chat))
            }
        }
        out.append(Data("}".utf8))
        return out
    }

    /// One entry of the file, read on its own: a chat this build cannot read
    /// back (a tool renamed since, a newer shape) costs that chat and not
    /// every other one in the file.
    private struct Lossy: Decodable {
        let saved: SavedChat?
        init(from decoder: Decoder) throws {
            saved = try? SavedChat(from: decoder)
        }
    }
}
