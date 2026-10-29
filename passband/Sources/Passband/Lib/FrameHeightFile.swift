// MEASURED EMAIL HEIGHTS, KEPT ACROSS LAUNCHES. The reader sizes an HTML
// message's frame from a height the offscreen pass measured (FrameMeasurer),
// and that pass ran again for every message on every launch: one WebKit load
// per message, remote images and all, because `FrameHeights` lived in memory
// only. This file keeps the settled answers, per account.
//
// A HEIGHT IS AN ANSWER TO A PRECISE QUESTION, and the file keeps the question
// with it so a stale answer is never used:
// - the build and the reader's type and padding (`ReaderMetrics`), once for
//   the whole file: the reader's CSS ships in the build, and a density
//   change re-sizes everything;
// - per height, the column width and window height it was taken at (part of
//   the entry's key, so a column narrowed by a side panel files its own
//   heights beside the full-width ones), and a signature of the document
//   measured: its HTML, its attachments (cid images), and whether trackers
//   and remote images were allowed. A body fetched again after a sanitizer
//   change, or translated since, is measured again.
// Any of those differs, and the height is not on file. A height is kept only
// when it SETTLED with every image drawn (FrameMeasurer): one taken while a
// host was failing would otherwise be the wrong size on every later launch.
//
// WHAT IS IN IT: frame keys (a message id, its style, its version) and
// numbers. No mail content. It is still written the way the board file is
// (PrivateFile: the owner's alone, out of backups) and deleted with the
// account.

import Foundation

struct FrameHeightFile: Codable, Equatable, Sendable {
    /// The shape of the file. Another version is not read.
    static let currentVersion = 1
    /// At most this many heights are kept; the least recently used go first.
    /// About 70 bytes each.
    static let capacity = 4000

    var version = FrameHeightFile.currentVersion
    var build: String
    var metrics: String
    var heights: [String: Entry] = [:]

    struct Entry: Codable, Equatable, Sendable {
        var height: Double
        var signature: UInt64
        /// When it was last measured or read, for the cap.
        var used: Double
    }

    /// Whether this file's heights answer questions asked under these.
    func holds(build: String, metrics: String) -> Bool {
        version == Self.currentVersion && self.build == build && self.metrics == metrics
    }

    /// Where a height for `key` taken at this column width and window height
    /// is filed. Whole points: a width a fraction off is the same column.
    static func slot(_ key: String, width: Double, viewport: Double) -> String {
        "\(key)@\(Int(width.rounded()))x\(Int(viewport.rounded()))"
    }

    /// The height on file for this exact document, if there is one.
    func height(_ slot: String, signature: UInt64) -> Double? {
        guard let entry = heights[slot], entry.signature == signature, entry.height > 0 else {
            return nil
        }
        return entry.height
    }

    /// A height was used: it is the last to go under the cap.
    mutating func touch(_ slot: String, at now: Double) {
        heights[slot]?.used = now
    }

    /// File a settled height, keeping the file under `capacity`.
    mutating func record(_ slot: String, height: Double, signature: UInt64, at now: Double) {
        guard height > 0 else { return }
        heights[slot] = Entry(height: height, signature: signature, used: now)
        guard heights.count > Self.capacity else { return }
        let overflow = heights.count - Self.capacity
        for old in heights.sorted(by: { $0.value.used < $1.value.used }).prefix(overflow) {
            heights.removeValue(forKey: old.key)
        }
    }

    /// The document a height belongs to: its HTML, whether remote images
    /// were drawn, and `extra`, everything else the prepared document is made
    /// from (trackers allowed, the attachments a cid image resolves to).
    /// FNV-1a over the bytes, because Swift's own hashing is seeded per
    /// process and a signature has to mean the same thing on the next launch.
    static func signature(html: String, allowRemote: Bool, extra: String = "") -> UInt64 {
        var hash: UInt64 = 0xcbf2_9ce4_8422_2325
        func mix(_ byte: UInt8) {
            hash ^= UInt64(byte)
            hash = hash &* 0x0000_0100_0000_01b3
        }
        for byte in html.utf8 { mix(byte) }
        mix(allowRemote ? 1 : 2)
        if !extra.isEmpty {
            mix(0)
            for byte in extra.utf8 { mix(byte) }
        }
        return hash
    }

    // MARK: - on disk

    static var directory: URL {
        let base =
            FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first
            ?? FileManager.default.temporaryDirectory
        return base.appendingPathComponent("Passband/FrameHeights", isDirectory: true)
    }

    static func url(account: UUID, in directory: URL = FrameHeightFile.directory) -> URL {
        directory.appendingPathComponent("heights-\(account.uuidString).json")
    }

    /// This account's file, or nil when there is none or it cannot be read.
    /// A file that cannot be read is deleted: it would never be read.
    static func load(account: UUID, in directory: URL = FrameHeightFile.directory) -> FrameHeightFile? {
        let url = url(account: account, in: directory)
        guard let data = try? Data(contentsOf: url) else { return nil }
        guard let file = try? JSONDecoder().decode(FrameHeightFile.self, from: data),
            file.version == currentVersion
        else {
            try? FileManager.default.removeItem(at: url)
            return nil
        }
        return file
    }

    static func write(_ file: FrameHeightFile, account: UUID, in directory: URL = FrameHeightFile.directory)
        throws
    {
        let data = try JSONEncoder().encode(file)
        try PrivateFile.write(data, to: url(account: account, in: directory), in: directory)
    }

    /// The account is gone (removed, or reconnected to another server).
    static func forget(account: UUID, in directory: URL = FrameHeightFile.directory) {
        try? FileManager.default.removeItem(at: url(account: account, in: directory))
    }

    /// Delete every file that is not one of these accounts', and anything
    /// else in the directory.
    static func sweep(keeping accounts: Set<UUID>, in directory: URL = FrameHeightFile.directory) {
        let keep = Set(accounts.map { url(account: $0, in: directory).lastPathComponent })
        let names = (try? FileManager.default.contentsOfDirectory(atPath: directory.path)) ?? []
        for name in names where !keep.contains(name) {
            try? FileManager.default.removeItem(at: directory.appendingPathComponent(name))
        }
    }
}
