// CALENDAR DISK CACHE: the last calendar feed each account loaded, kept across
// relaunch and account switch, so the Calendar page paints its events on the
// first frame instead of showing "loading your calendar…" while the daemon
// answers. The page still asks the daemon every time it opens; this is only
// what it shows until that answer lands.
//
// The feed is the user's calendar (titles, places, descriptions, guests'
// addresses): content at rest, kept by the rules docs/SECURITY.md §6 sets for
// the saved chats and the last board. Written through `PrivateFile` (one file
// per account under Application Support's Passband/CalendarCache, excluded
// from backup, atomic, 0600 in a 0700 directory on the Mac, complete file
// protection on the phone), contents never logged. A feed saved more than a
// week ago is not painted and is deleted, by the read that finds it or by the
// launch sweep, which also deletes the feed of every account no longer on the
// install. The file goes when the account is removed, when the calendar gate
// closes, when the same daemon is connected again or re-validated, and when
// the daemon refuses the saved credentials. Never written for the practice
// inbox. A file that will not decode is deleted and read as nothing.

import CryptoKit
import Foundation

actor CalendarDisk {
    static let shared = CalendarDisk(
        dir: (FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)
            .first ?? URL(fileURLWithPath: NSTemporaryDirectory()))
            .appendingPathComponent("Passband/CalendarCache", isDirectory: true))

    /// Bumped whenever the stored shape changes: an older file is dropped
    /// rather than half-read.
    static let version = 2

    /// How long a saved feed may be painted: a week, as the last board.
    static let maxAge: TimeInterval = 7 * 86_400

    private struct Stored: Codable {
        let v: Int
        let saved: Date
        let feed: TimelineFeed
    }

    private let dir: URL

    init(dir: URL) {
        self.dir = dir
    }

    private func name(_ account: UUID) -> String {
        SHA256.hash(data: Data(account.uuidString.utf8))
            .map { String(format: "%02x", $0) }.joined() + ".json"
    }

    private func url(_ account: UUID) -> URL {
        dir.appendingPathComponent(name(account))
    }

    /// The feed last saved for `account`, or nil (none, unreadable, or older
    /// than `maxAge`, which are deleted).
    func feed(for account: UUID, now: Date = Date()) -> TimelineFeed? {
        let file = url(account)
        guard let data = try? Data(contentsOf: file) else { return nil }
        guard let stored = try? JSONDecoder().decode(Stored.self, from: data),
              stored.v == Self.version,
              now.timeIntervalSince(stored.saved) < Self.maxAge
        else {
            try? FileManager.default.removeItem(at: file)
            return nil
        }
        return stored.feed
    }

    /// Keep `feed` as `account`'s, replacing what was there.
    func save(_ feed: TimelineFeed, for account: UUID, now: Date = Date()) {
        guard let data = try? JSONEncoder().encode(Stored(v: Self.version, saved: now, feed: feed))
        else { return }
        // A cache that cannot be written is a page that loads the old way.
        try? PrivateFile.write(data, to: url(account), in: dir)
    }

    /// Delete `account`'s feed.
    func forget(_ account: UUID) {
        try? FileManager.default.removeItem(at: url(account))
    }

    /// The launch sweep: delete every feed older than `maxAge`, every feed of
    /// an account not in `accounts`, and anything else in the directory (it is
    /// this cache's alone; an interrupted atomic write leaves calendar content
    /// behind too).
    func sweep(keeping accounts: Set<UUID>, now: Date = Date()) {
        let kept = Set(accounts.map(name))
        let names = (try? FileManager.default.contentsOfDirectory(atPath: dir.path)) ?? []
        for file in names {
            let url = dir.appendingPathComponent(file)
            let written = (try? FileManager.default.attributesOfItem(atPath: url.path))?[.modificationDate] as? Date
            let stale = written.map { now.timeIntervalSince($0) >= Self.maxAge } ?? false
            if stale || !kept.contains(file) { try? FileManager.default.removeItem(at: url) }
        }
    }
}
