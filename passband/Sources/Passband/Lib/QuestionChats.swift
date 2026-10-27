// THE CHAT BEHIND EACH QUESTION IN SEARCH HISTORY. A question in the ring
// (RecentSearchStore) is the first message of a conversation the reader may
// have worked at for a while, narrowing and following up until the agent found
// the thing. Tapping it should open THAT conversation, not start the digging
// over, so the conversation is kept here beside the line that names it.
//
// SAME SCOPE AS THE RING: device-local, per ACCOUNT, keyed by the question's
// ring identity, and never longer-lived than its line. Whenever the ring
// changes, chats whose question has fallen out of it go too; clearing history
// clears these with it.
//
// A FILE, NOT USER DEFAULTS. A transcript is mail content: subjects, senders,
// snippets, whole tool results. It lives in Application Support, written
// atomically, and on the phone under complete file protection, so it is
// unreadable while the device is locked. Its contents are never logged.

import Foundation
import os

@MainActor
final class QuestionChats {
    static let shared = QuestionChats()

    /// The largest single conversation worth keeping, encoded. A chat bigger
    /// than this is one long enough that asking again is the better deal than
    /// carrying it on disk, and it bounds the file at ring capacity times this.
    static let maxBytes = 1_000_000

    private static let log = Logger(subsystem: "app.passband.client", category: "question-chats")

    /// Saved chats for the live account, by ring identity.
    private var chats: [String: ConversationSnapshot] = [:]

    private init() { reload() }

    // MARK: - keys

    /// The key a question's chat is kept under: the ring's own identity for
    /// the question's stored line, so "Where is my flight" and "where is my
    /// flight " are one chat for the same reason they are one line.
    static func key(forQuestion text: String) -> String {
        RecentSearchRing.identity(of: RecentEntry(kind: .question, text: text).stored)
    }

    // MARK: - reads

    func snapshot(forQuestion text: String) -> ConversationSnapshot? {
        chats[Self.key(forQuestion: text)]
    }

    func snapshot(forKey key: String) -> ConversationSnapshot? { chats[key] }

    // MARK: - writes

    /// Keep `snapshot` as the chat for `key`. Refused when the account is gone,
    /// when the question is no longer in the ring (nothing could ever open
    /// it), and when it is over `maxBytes`, in which case an older, smaller
    /// copy is dropped too: it would restore a conversation the reader has
    /// since moved past.
    func save(_ snapshot: ConversationSnapshot, forKey key: String) {
        guard Self.fileURL != nil, Self.ringKeys().contains(key) else { return }
        let size: Int
        do {
            size = try snapshot.encoded().count
        } catch {
            Self.log.error("question chat did not encode")
            return
        }
        guard size <= Self.maxBytes else {
            Self.log.notice("question chat not saved: \(size, privacy: .public) bytes")
            if chats.removeValue(forKey: key) != nil { write() }
            return
        }
        chats[key] = snapshot
        write()
    }

    /// Drop every chat whose question has left the ring. Called by the ring
    /// whenever it changes.
    func prune() {
        let live = Self.ringKeys()
        let before = chats.count
        chats = chats.filter { live.contains($0.key) }
        if chats.count != before { write() }
    }

    /// Forget every chat for the live account (history was cleared).
    func clear() {
        chats = [:]
        guard let url = Self.fileURL else { return }
        try? FileManager.default.removeItem(at: url)
    }

    /// Re-read for whatever account is live NOW, beside the ring's own reload.
    func reload() {
        guard let url = Self.fileURL, let data = try? Data(contentsOf: url),
            let file = try? JSONDecoder().decode([String: Lossy].self, from: data)
        else {
            chats = [:]
            return
        }
        chats = file.compactMapValues(\.snapshot)
        // A line can leave the ring while this file could not be written (or
        // under a build that kept no chats), so the file may name one.
        prune()
    }

    /// Delete one account's file outright: that account is being forgotten.
    /// Not main-actor bound, because the account index's cleanup is not.
    nonisolated static func forget(account id: UUID) {
        try? FileManager.default.removeItem(at: fileURL(for: id))
    }

    // MARK: - storage

    /// One entry of the file, read on its own: a chat this build cannot read
    /// back (a tool renamed since, a newer shape) costs that chat and not
    /// every other one in the file.
    private struct Lossy: Decodable {
        let snapshot: ConversationSnapshot?
        init(from decoder: Decoder) throws {
            snapshot = try? ConversationSnapshot(from: decoder)
        }
    }

    private static var fileURL: URL? {
        AccountManager.shared.activeId.map(fileURL(for:))
    }

    nonisolated private static func fileURL(for id: UUID) -> URL {
        let base =
            FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first
            ?? URL(fileURLWithPath: NSTemporaryDirectory())
        return base.appendingPathComponent("Passband/QuestionChats", isDirectory: true)
            .appendingPathComponent(AccountIndex.scopedKey("chats", id) + ".json")
    }

    /// The ring identities of every QUESTION still in history.
    private static func ringKeys() -> Set<String> {
        Set(
            RecentSearchStore.shared.entries
                .filter { $0.kind == .question }
                .map { key(forQuestion: $0.text) })
    }

    private func write() {
        guard let url = Self.fileURL else { return }
        if chats.isEmpty {
            try? FileManager.default.removeItem(at: url)
            return
        }
        do {
            try FileManager.default.createDirectory(
                at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
            let data = try JSONEncoder().encode(chats)
            #if os(iOS)
                try data.write(to: url, options: [.atomic, .completeFileProtection])
            #else
                try data.write(to: url, options: .atomic)
            #endif
        } catch {
            Self.log.error("question chats not written: \(error.localizedDescription, privacy: .public)")
        }
    }
}
