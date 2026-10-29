// CALENDAR DISK CACHE: the last calendar feed each account loaded, kept across
// relaunch and account switch, so the Calendar page paints its events on the
// first frame instead of showing "loading your calendar…" while the daemon
// answers. The page still asks the daemon every time it opens; this is only
// what it shows until that answer lands.
//
// One JSON file per account, named by a hash of the account's id, under
// Application Support/Passband/CalendarCache, excluded from backups. The
// feed is the user's own calendar (titles, places, guests), so it lives only
// as long as its account and the calendar feature: `forget` runs when an
// account is removed and when the calendar gate closes. Reads and writes run
// off the main actor; a file that will not decode is deleted and read as
// nothing.

import CryptoKit
import Foundation

actor CalendarDisk {
    static let shared = CalendarDisk(
        dir: (FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)
            .first ?? URL(fileURLWithPath: NSTemporaryDirectory()))
            .appendingPathComponent("Passband/CalendarCache", isDirectory: true))

    /// Bumped whenever the stored shape changes: an older file is dropped
    /// rather than half-read.
    static let version = 1

    private struct Stored: Codable {
        let v: Int
        let feed: TimelineFeed
    }

    private let dir: URL

    init(dir: URL) {
        self.dir = dir
    }

    private func url(_ account: UUID) -> URL {
        let name = SHA256.hash(data: Data(account.uuidString.utf8))
            .map { String(format: "%02x", $0) }.joined()
        return dir.appendingPathComponent(name + ".json")
    }

    /// The feed last saved for `account`, or nil.
    func feed(for account: UUID) -> TimelineFeed? {
        let file = url(account)
        guard let data = try? Data(contentsOf: file) else { return nil }
        guard let stored = try? JSONDecoder().decode(Stored.self, from: data),
              stored.v == Self.version
        else {
            try? FileManager.default.removeItem(at: file)
            return nil
        }
        return stored.feed
    }

    /// Keep `feed` as `account`'s, replacing what was there. Atomic: a crash
    /// mid-write leaves the old file, never half of the new one.
    func save(_ feed: TimelineFeed, for account: UUID) {
        guard let data = try? JSONEncoder().encode(Stored(v: Self.version, feed: feed)) else { return }
        do {
            try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
            var folder = dir
            var values = URLResourceValues()
            values.isExcludedFromBackup = true
            try? folder.setResourceValues(values)
            #if os(iOS)
            try data.write(to: url(account), options: [.atomic, .completeFileProtectionUntilFirstUserAuthentication])
            #else
            try data.write(to: url(account), options: .atomic)
            #endif
        } catch {
            // A cache that cannot be written is a page that loads the old way.
        }
    }

    /// Delete `account`'s feed.
    func forget(_ account: UUID) {
        try? FileManager.default.removeItem(at: url(account))
    }
}
