// THE LAST BOARD, KEPT ON DISK, so a launch paints the mailbox as it last stood
// before the daemon has answered anything. Without it every launch was a
// spinner for the keychain read, then the capabilities probe's round trip,
// then the first pull (feed, stats and sealed together, which on a large
// mailbox with a cold daemon is seconds of server time), and only then a row.
// With it the board is on screen from the file, marked as a snapshot
// (`AppStore.boardIsSnapshot`), and the first pull replaces it exactly as it
// replaces a board repainted on an account switch.
//
// WHAT IS KEPT is `PersistedBoard`: the bands, the calendar and shipment zones,
// the stats, the hero's headline, the head of each mail page, and when the
// daemon last answered for them. WHAT IS NEVER KEPT is not in that type at
// all, so no code path can write it: the sealed list (login codes), an auth
// row from any band or page (`AttentionUpdate.isAuthRow`: its preview can be
// the code itself), and the money zones, banking and receipts. Those repaint
// from the network.
//
// THE SAME PROTECTIONS AS THE SAVED CHATS (QuestionChats.swift), because this
// is mail content too: subjects, senders, the triage's one-line summaries.
// Application Support, excluded from backup, written atomically on one serial
// queue, on the Mac readable by the owner alone (0600 in a 0700 directory),
// on the phone under complete file protection, so it is unreadable while the
// device is locked (a launch then simply waits for the network, as before).
// Its contents are never logged. One file per account; it goes when the
// account is removed (`forget`), and when the same daemon is connected again
// with credentials that may open a different mailbox (`discard`). A board the
// daemon last answered for more than `PersistedBoard.maxAge` ago is not
// painted, and its file is deleted on the read that finds it so. See
// docs/SECURITY.md §6.

import Foundation
import os

/// What of a board is kept on disk. See the file header for what is not, and
/// why it cannot be.
struct PersistedBoard: Codable, Sendable, Equatable {
    /// The shape this build writes and reads. A file of another version is
    /// not painted (and is deleted): the next save writes this one.
    static let currentVersion = 1
    /// How old a board may be, by the daemon's last answer, and still be
    /// painted at launch: a week. Older than that it is not the mailbox the
    /// person left, it is a different one, and the network's answer is worth
    /// the wait.
    static let maxAge: TimeInterval = 7 * 86_400
    /// How far in the FUTURE a last answer may claim to be (a clock that
    /// stepped back since it was written) and still count as fresh.
    static let clockSlack: TimeInterval = 3_600

    var version = PersistedBoard.currentVersion
    var standing: [AttentionUpdate]
    var new: [AttentionUpdate]
    var open: [AttentionUpdate]
    var totalCount: Int?
    var stats: StoreStats?
    var fyeHeadline: String?
    var fyeHeadlineKey: String?
    var calendar: [CalendarUpdate]
    var shipments: [Shipment]
    /// The head of each mail page, keyed by `MailMode`'s raw value.
    var mailPages: [String: [AttentionUpdate]]
    var mailKeyset: Bool
    /// When the daemon last answered for this board: what the stale banner
    /// dates the board from, and what ages it out.
    var lastRefresh: Date

    /// A board to keep, with every auth row taken out of the bands and the
    /// pages on the way in.
    init(
        standing: [AttentionUpdate], new: [AttentionUpdate], open: [AttentionUpdate],
        totalCount: Int?, stats: StoreStats?, fyeHeadline: String?, fyeHeadlineKey: String?,
        calendar: [CalendarUpdate], shipments: [Shipment],
        mailPages: [String: [AttentionUpdate]], mailKeyset: Bool, lastRefresh: Date
    ) {
        self.standing = Self.withoutAuth(standing)
        self.new = Self.withoutAuth(new)
        self.open = Self.withoutAuth(open)
        self.totalCount = totalCount
        self.stats = stats
        self.fyeHeadline = fyeHeadline
        self.fyeHeadlineKey = fyeHeadlineKey
        self.calendar = calendar
        self.shipments = shipments
        self.mailPages = mailPages.mapValues(Self.withoutAuth)
        self.mailKeyset = mailKeyset
        self.lastRefresh = lastRefresh
    }

    static func withoutAuth(_ rows: [AttentionUpdate]) -> [AttentionUpdate] {
        rows.filter { !$0.isAuthRow }
    }

    /// Whether this board may be painted at `now`.
    func isFresh(now: Date) -> Bool {
        version == Self.currentVersion
            && lastRefresh <= now.addingTimeInterval(Self.clockSlack)
            && now.timeIntervalSince(lastRefresh) < Self.maxAge
    }

    static func encode(_ board: PersistedBoard) throws -> Data {
        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .secondsSince1970
        return try encoder.encode(board)
    }

    /// The board in `data`, if it may be painted at `now`. nil for a file that
    /// does not decode, is another version, or is stale. Auth rows are taken
    /// out again on the way back in, so a file written by anything else still
    /// cannot put one on screen from disk.
    static func decode(_ data: Data, now: Date) -> PersistedBoard? {
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .secondsSince1970
        guard var board = try? decoder.decode(PersistedBoard.self, from: data),
            board.isFresh(now: now)
        else { return nil }
        board.standing = withoutAuth(board.standing)
        board.new = withoutAuth(board.new)
        board.open = withoutAuth(board.open)
        board.mailPages = board.mailPages.mapValues(withoutAuth)
        return board
    }
}

/// Where the boards are kept, and every read and write of them. All on one
/// serial queue, so a read is ordered after the writes before it and the last
/// save is the last write.
enum BoardFile {
    private static let log = Logger(subsystem: "app.passband.client", category: "board-file")

    private static let queue = DispatchQueue(label: "app.passband.board-file", qos: .utility)

    /// Accounts forgotten this session: a write already queued for one is
    /// dropped, so its file cannot come back after the delete.
    private static let forgotten = OSAllocatedUnfairLock(initialState: Set<UUID>())

    static func directory() -> URL {
        let base =
            FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first
            ?? URL(fileURLWithPath: NSTemporaryDirectory())
        return base.appendingPathComponent("Passband/Boards", isDirectory: true)
    }

    static func fileURL(for id: UUID, in directory: URL) -> URL {
        directory.appendingPathComponent("board-\(id.uuidString).json")
    }

    /// The board kept for `id`, if there is one that may be painted now. A
    /// file that is there but stale, of another version, or corrupt is
    /// deleted: nothing will ever paint it. One that cannot be read at all
    /// (the phone locked) is left for a later launch.
    static func load(
        account id: UUID, now: Date = Date(), in directory: URL = BoardFile.directory()
    ) async -> PersistedBoard? {
        let url = fileURL(for: id, in: directory)
        return await withCheckedContinuation { continuation in
            queue.async { continuation.resume(returning: read(url, now: now)) }
        }
    }

    /// Keep `board` for `id`. Encoded and written on the queue, off the main
    /// actor. `wait`: return only once it is on disk (the Mac quitting).
    static func save(
        _ board: PersistedBoard, account id: UUID, wait: Bool = false,
        in directory: URL = BoardFile.directory()
    ) {
        let url = fileURL(for: id, in: directory)
        let job: @Sendable () -> Void = { write(board, to: url, in: directory, account: id) }
        if wait { queue.sync(execute: job) } else { queue.async(execute: job) }
    }

    /// Delete `id`'s board, leaving later saves free to write a new one: the
    /// same daemon connected again, whose credentials may open another
    /// mailbox behind it.
    static func discard(account id: UUID, in directory: URL = BoardFile.directory()) {
        let url = fileURL(for: id, in: directory)
        queue.async { remove(url) }
    }

    /// Delete `id`'s board for good: the account is being forgotten. A write
    /// already queued for it is skipped, and the delete is queued again behind
    /// any in flight, so the file cannot be recreated.
    static func forget(account id: UUID, in directory: URL = BoardFile.directory()) {
        forgotten.withLock { _ = $0.insert(id) }
        let url = fileURL(for: id, in: directory)
        remove(url)
        queue.async { remove(url) }
    }

    // MARK: - on the queue

    private static func read(_ url: URL, now: Date) -> PersistedBoard? {
        guard FileManager.default.fileExists(atPath: url.path) else { return nil }
        guard let data = try? Data(contentsOf: url) else { return nil }
        guard let board = PersistedBoard.decode(data, now: now) else {
            remove(url)
            return nil
        }
        return board
    }

    private static func write(_ board: PersistedBoard, to url: URL, in directory: URL, account id: UUID) {
        guard !forgotten.withLock({ $0.contains(id) }) else { return }
        do {
            let data = try PersistedBoard.encode(board)
            var dir = directory
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
            log.error("board not written: \(error.localizedDescription, privacy: .public)")
        }
    }

    private static func remove(_ url: URL) {
        try? FileManager.default.removeItem(at: url)
    }
}

/// Launch to the first board on screen, as one Instruments interval: "first
/// board" under Points of Interest, the client measurement
/// docs/PERFORMANCE.md §4 asks for. Begun when the store is made and ended
/// once, by whichever paints first: the board from disk, or the first pull to
/// land. Its end message says which, so one build measures both launches (a
/// launch with no board kept is the network's).
@MainActor
enum LaunchSignpost {
    private static let signposter = OSSignposter(
        subsystem: "app.passband.client", category: .pointsOfInterest)
    private static var interval: OSSignpostIntervalState?

    static var isOpen: Bool { interval != nil }

    static func begin() {
        guard interval == nil else { return }
        interval = signposter.beginInterval("first board")
    }

    static func end(fromDisk: Bool) {
        guard let state = interval else { return }
        interval = nil
        if fromDisk {
            signposter.endInterval("first board", state, "from disk")
        } else {
            signposter.endInterval("first board", state, "from the network")
        }
    }
}
