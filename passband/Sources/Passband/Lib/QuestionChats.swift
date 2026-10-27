// THE CHAT BEHIND EACH QUESTION IN SEARCH HISTORY. A question in the ring
// (RecentSearchStore) is the first message of a conversation the reader may
// have worked at for a while, narrowing and following up until the agent found
// the thing. Tapping it should open THAT conversation, not start the digging
// over, so the conversation is kept here beside the line that names it.
//
// SAME SCOPE AS THE RING: device-local, per ACCOUNT, keyed by the question's
// ring identity, and never longer-lived than its line. Whenever the ring
// changes, chats whose question has fallen out of it go too; clearing history
// clears these with it. And never longer than `SavedChat.maxAge` (30 days),
// whatever the ring says.
//
// A FILE, NOT USER DEFAULTS. A transcript is mail content: subjects, senders,
// snippets, whole tool results. It lives in Application Support, excluded from
// backup, written atomically, on the Mac readable by the owner alone (0600 in
// a 0700 directory), and on the phone under complete file protection, so it is
// unreadable while the device is locked. Its contents are never logged. See
// docs/SECURITY.md §6.
//
// ONE ACCOUNT'S CHATS NEVER LAND IN ANOTHER'S FILE. The in-memory chats belong
// to the account they were loaded for (`loadedFor`), and every write requires
// that to still be the live account. An account switch commits the new id
// before the ring and these reload, and a run of the old account's can end in
// between; its save is dropped rather than written under the new account.
//
// UNREADABLE IS NOT EMPTY. A file that is there but does not read (the phone
// locked, a corrupt file) leaves the chats unloaded and every write refused, so
// nothing in it is overwritten by the one chat saved next. The load is retried
// on the next read or save, and on the phone when protected data comes back.

import Foundation
import os

#if os(iOS)
    import UIKit
#endif

@MainActor
final class QuestionChats {
    static let shared = QuestionChats()

    /// The largest single conversation worth keeping, encoded. A chat bigger
    /// than this is one long enough that asking again is the better deal than
    /// carrying it on disk, and it bounds the file at ring capacity times this.
    static let maxBytes = 1_000_000

    nonisolated private static let log = Logger(subsystem: "app.passband.client", category: "question-chats")

    /// Saved chats for `loadedFor`, by ring identity.
    private var chats: [String: SavedChat] = [:]
    /// Each chat's encoded bytes, for the ones encoded this session: a save
    /// encodes its chat once, on the main actor, and the file is assembled
    /// from these. Chats read from disk are encoded by the writer instead.
    private var encoded: [String: Data] = [:]
    /// The account `chats` were loaded for. nil: none (no live account).
    private var loadedFor: UUID?
    /// `loadedFor`'s file is there but did not read. Writes are refused until
    /// a retry reads it.
    private var unreadable = false
    /// The account whose corrupt file has already been reported, so a file
    /// that will never read is logged once rather than on every retry.
    private var reportedCorrupt: UUID?

    /// Every file write, in order, off the main actor. Serial, so the last
    /// save is the last write.
    nonisolated private static let writer = DispatchQueue(
        label: "app.passband.question-chats", qos: .utility)

    private init() {
        reload()
        #if os(iOS)
            NotificationCenter.default.addObserver(
                forName: UIApplication.protectedDataDidBecomeAvailableNotification, object: nil,
                queue: .main
            ) { _ in
                MainActor.assumeIsolated { QuestionChats.shared.retryIfUnreadable() }
            }
        #endif
    }

    // MARK: - keys

    /// The key a question's chat is kept under: the ring's own identity for
    /// the question's stored line, so "Where is my flight" and "where is my
    /// flight " are one chat for the same reason they are one line.
    static func key(forQuestion text: String) -> String {
        RecentSearchRing.identity(of: RecentEntry(kind: .question, text: text).stored)
    }

    // MARK: - reads

    func snapshot(forQuestion text: String) -> ConversationSnapshot? {
        snapshot(forKey: Self.key(forQuestion: text))
    }

    /// The saved chat for `key`, if it is the live account's and not expired.
    func snapshot(forKey key: String) -> ConversationSnapshot? {
        guard let id = liveLoadedAccount else { return nil }
        retryIfUnreadable()
        guard !unreadable, loadedFor == id, let saved = chats[key],
            saved.isFresh(now: Date())
        else { return nil }
        return saved.snapshot
    }

    // MARK: - writes

    /// Keep `snapshot` as the chat for `key`. Refused when the account is gone
    /// or is not the one these chats were loaded for, when the file could not
    /// be read, when the question is no longer in the ring (nothing could ever
    /// open it), and when it is over `maxBytes`, in which case an older,
    /// smaller copy is dropped too: it would restore a conversation the reader
    /// has since moved past.
    func save(_ snapshot: ConversationSnapshot, forKey key: String) {
        guard liveLoadedAccount != nil else { return }
        retryIfUnreadable()
        guard !unreadable, Self.ringKeys().contains(key) else { return }
        let saved = SavedChat(savedAt: Date(), snapshot: snapshot)
        let bytes: Data
        do {
            bytes = try SavedChat.encodeEntry(saved)
        } catch {
            Self.log.error("question chat did not encode")
            return
        }
        guard bytes.count <= Self.maxBytes else {
            Self.log.notice("question chat not saved: \(bytes.count, privacy: .public) bytes")
            if chats.removeValue(forKey: key) != nil {
                encoded[key] = nil
                write()
            }
            return
        }
        chats[key] = saved
        encoded[key] = bytes
        write()
    }

    /// Drop every chat whose question has left the ring, or that has expired.
    /// Called by the ring whenever it changes.
    func prune() {
        guard liveLoadedAccount != nil, !unreadable else { return }
        if dropDead() { write() }
    }

    /// Forget every chat for the live account (history was cleared). Allowed
    /// with an unreadable file: deleting it is what clearing means, and it is
    /// also the way out of a file that will never read.
    func clear() {
        guard let id = liveLoadedAccount else { return }
        chats = [:]
        encoded = [:]
        unreadable = false
        let url = Self.fileURL(for: id)
        Self.writer.async { Self.remove(url) }
    }

    /// Re-read for whatever account is live NOW, beside the ring's own reload.
    func reload() {
        chats = [:]
        encoded = [:]
        unreadable = false
        loadedFor = AccountManager.shared.activeId
        load()
        // A line can leave the ring while this file could not be written (or
        // under a build that kept no chats), so the file may name one.
        prune()
    }

    /// Delete one account's file outright: that account is being forgotten.
    /// Not main-actor bound, because the account index's cleanup is not. A
    /// write already queued for it is skipped, and the delete is queued again
    /// behind any in flight, so the file cannot be recreated.
    nonisolated static func forget(account id: UUID) {
        forgotten.withLock { _ = $0.insert(id) }
        let url = fileURL(for: id)
        remove(url)
        writer.async { remove(url) }
    }

    // MARK: - loading

    /// The live account, when it is the one the chats were loaded for.
    private var liveLoadedAccount: UUID? {
        guard let id = AccountManager.shared.activeId, id == loadedFor else { return nil }
        return id
    }

    /// Read `loadedFor`'s file. No file is no chats; a file that does not
    /// read leaves `unreadable` set.
    private func load() {
        guard let id = loadedFor else { return }
        let url = Self.fileURL(for: id)
        guard FileManager.default.fileExists(atPath: url.path) else {
            unreadable = false
            return
        }
        let data: Data
        do {
            data = try Data(contentsOf: url)
        } catch {
            // Locked (complete protection) or otherwise not ours to read
            // right now. Not logged per retry: this is the phone's normal
            // state while locked.
            unreadable = true
            return
        }
        guard let file = SavedChat.readFile(data, now: Date()) else {
            unreadable = true
            if reportedCorrupt != id {
                reportedCorrupt = id
                Self.log.error("question chats file did not read; keeping it, saving nothing")
            }
            return
        }
        chats = file
        unreadable = false
    }

    /// Try the load again if the last one could not read the file. Cheap when
    /// it could: one flag.
    private func retryIfUnreadable() {
        guard unreadable, liveLoadedAccount != nil else { return }
        load()
        if !unreadable, dropDead() { write() }
    }

    /// Drop chats whose question left the ring or that expired. True if any
    /// went.
    private func dropDead() -> Bool {
        let live = Self.ringKeys()
        let now = Date()
        let before = chats.count
        chats = chats.filter { live.contains($0.key) && $0.value.isFresh(now: now) }
        encoded = encoded.filter { chats[$0.key] != nil }
        return chats.count != before
    }

    // MARK: - storage

    nonisolated private static let forgotten = OSAllocatedUnfairLock(initialState: Set<UUID>())

    nonisolated private static func directory() -> URL {
        let base =
            FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first
            ?? URL(fileURLWithPath: NSTemporaryDirectory())
        return base.appendingPathComponent("Passband/QuestionChats", isDirectory: true)
    }

    nonisolated private static func fileURL(for id: UUID) -> URL {
        directory().appendingPathComponent(AccountIndex.scopedKey("chats", id) + ".json")
    }

    /// The ring identities of every QUESTION still in history.
    private static func ringKeys() -> Set<String> {
        Set(
            RecentSearchStore.shared.entries
                .filter { $0.kind == .question }
                .map { key(forQuestion: $0.text) })
    }

    /// Queue the whole file as it now stands. Only ever reached through a
    /// guard that `loadedFor` is live and readable.
    private func write() {
        guard let id = liveLoadedAccount, !unreadable else { return }
        let url = Self.fileURL(for: id)
        if chats.isEmpty {
            Self.writer.async { Self.remove(url) }
            return
        }
        // Chats read from disk have no bytes yet; the writer encodes those.
        var entries: [String: SavedChat.Entry] = [:]
        for (key, chat) in chats {
            entries[key] = encoded[key].map(SavedChat.Entry.bytes) ?? .chat(chat)
        }
        Self.writer.async { Self.write(entries, to: url, account: id) }
    }

    nonisolated private static func remove(_ url: URL) {
        try? FileManager.default.removeItem(at: url)
    }

    /// On the writer queue only.
    nonisolated private static func write(
        _ entries: [String: SavedChat.Entry], to url: URL, account id: UUID
    ) {
        guard !forgotten.withLock({ $0.contains(id) }) else { return }
        do {
            let data = try SavedChat.fileBytes(entries)
            var dir = directory()
            #if os(macOS)
                try FileManager.default.createDirectory(
                    at: dir, withIntermediateDirectories: true,
                    attributes: [.posixPermissions: 0o700])
                try? FileManager.default.setAttributes([.posixPermissions: 0o700], ofItemAtPath: dir.path)
            #else
                try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
            #endif
            var values = URLResourceValues()
            values.isExcludedFromBackup = true
            try? dir.setResourceValues(values)
            #if os(iOS)
                try data.write(to: url, options: [.atomic, .completeFileProtection])
            #else
                try data.write(to: url, options: .atomic)
                try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: url.path)
            #endif
        } catch {
            log.error("question chats not written: \(error.localizedDescription, privacy: .public)")
        }
    }
}
