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
// the hero's headline, the head of each mail page, when the daemon last
// answered for them (which ages the file out, and nothing else), and the ids
// (ids only) of the threads that hold auth mail, so the prefetch fence stands
// before the sealed list is fetched again. NOT the stats: the shell mounts on
// rows alone, and everything that waits for the daemon's first answer (the
// stats' capabilities, the first sync's prompts, the clock the stale banner
// reads) waits for it exactly as on a launch with no board.
//
// WHAT IS NEVER KEPT. The sealed list and the banking and receipt cards (the
// amounts and balances extracted from that mail) have no field at all; that
// mail's own rows on a mail page are kept like any other mail. AUTH MAIL is
// kept out by every signal there is for it,
// because a row's preview can be the code itself: the row's own decision
// (`auth_kinds`, which only the feed's rows carry), its zone (`placement`
// "auth", which the mail pages' rows carry), and the threads the store already
// fences (`AppStore.authThreadIds`: the sealed list and the bands' decisions).
// Every row of a thread any of those names goes, from every band, page and
// zone. A mail-page row triage has not placed yet goes too: it is the newest
// mail, which is exactly where a code that just arrived sits, and nothing has
// said yet whether it is one. A page is cut at its first such row rather than
// holed, because the scroll pages on from a head's last row and would never
// refill a gap; a page cut to nothing is not kept, so it paints as loading,
// not as empty. The rules run on the way in and again on the way back out, so
// a file written any other way still cannot paint auth mail.
//
// THE SAME PROTECTIONS AS THE SAVED CHATS (`PrivateFile`), because this is mail
// content too: subjects, senders, the triage's one-line summaries. Application
// Support, excluded from backup, written atomically on one serial queue, on
// the Mac readable by the owner alone (0600 in a 0700 directory), on the phone
// under complete file protection, so it is unreadable while the device is
// locked (a launch then simply waits for the network, as before). Its contents
// are never logged. One file per account; it goes when the account is removed
// (`forget`), when the same daemon is connected again or re-validated with
// credentials that may open a different mailbox (`discard`), and when the
// daemon refuses the saved credentials. A board the daemon last answered for
// more than `PersistedBoard.maxAge` ago is not painted and is deleted: by the
// read that finds it so, or by the launch sweep (`sweep`), which also deletes
// the boards of accounts no longer on this install. See docs/SECURITY.md §6.

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
    /// For your eyes, the one band the poller fills.
    var standing: [AttentionUpdate]
    var totalCount: Int?
    var fyeHeadline: String?
    var fyeHeadlineKey: String?
    var calendar: [CalendarUpdate]
    var shipments: [Shipment]
    /// The head of each mail page, keyed by `MailMode`'s raw value.
    var mailPages: [String: [AttentionUpdate]]
    var mailKeyset: Bool
    /// The threads that hold auth mail, sorted: what `ThreadPrefetch` refuses
    /// from the moment this board is painted. Ids, never content.
    var authThreads: [String]
    /// When the daemon last answered for this board: what ages it out. Not
    /// painted as the store's `lastRefresh`, which only a pull sets.
    var lastRefresh: Date

    /// A board to keep, with the auth mail taken out (see the file header).
    /// `authThreads` is the store's own fence; the rows' own signals add to
    /// it.
    init(
        standing: [AttentionUpdate], totalCount: Int?, fyeHeadline: String?, fyeHeadlineKey: String?,
        calendar: [CalendarUpdate], shipments: [Shipment],
        mailPages: [String: [AttentionUpdate]], mailKeyset: Bool,
        authThreads: Set<String>, lastRefresh: Date
    ) {
        self.standing = standing
        self.totalCount = totalCount
        self.fyeHeadline = fyeHeadline
        self.fyeHeadlineKey = fyeHeadlineKey
        self.calendar = calendar
        self.shipments = shipments
        self.mailPages = mailPages
        self.mailKeyset = mailKeyset
        self.authThreads = authThreads.sorted()
        self.lastRefresh = lastRefresh
        withoutAuthMail()
    }

    /// Whether this board may be painted at `now`.
    func isFresh(now: Date) -> Bool {
        version == Self.currentVersion
            && lastRefresh <= now.addingTimeInterval(Self.clockSlack)
            && now.timeIntervalSince(lastRefresh) < Self.maxAge
    }

    // MARK: - the auth rules

    /// Auth mail by the row's own word: its decision's kinds (feed rows) or
    /// its zone (mail-page rows).
    static func isAuthMail(_ row: AttentionUpdate) -> Bool {
        row.isAuthRow || row.placement == "auth"
    }

    /// A band row is kept unless it is auth mail or in a fenced thread.
    static func keepsBandRow(_ row: AttentionUpdate, fence: Set<String>) -> Bool {
        !isAuthMail(row) && !fence.contains(row.thread_id)
    }

    /// A mail-page row also needs a zone: one triage has not placed yet may be
    /// a code that just arrived. A page keeps its rows only down to the first
    /// that fails this (`withoutAuthMail`).
    static func keepsPageRow(_ row: AttentionUpdate, fence: Set<String>) -> Bool {
        row.placement != nil && keepsBandRow(row, fence: fence)
    }

    /// Take every auth row out, and every row of a thread that holds one,
    /// from the bands, the pages and the zones; cut each page at its first
    /// row that goes, and drop a page cut to nothing. The headline goes with
    /// any band row taken out: it was written about the band as it stood, and
    /// may be about the mail just removed.
    private mutating func withoutAuthMail() {
        var fence = Set(authThreads)
        var rows = standing
        for page in mailPages.values { rows += page }
        for row in rows where Self.isAuthMail(row) { fence.insert(row.thread_id) }
        fence.remove("")
        authThreads = fence.sorted()
        let before = standing.count
        standing = standing.filter { Self.keepsBandRow($0, fence: fence) }
        mailPages = mailPages
            .mapValues { rows in Array(rows.prefix(while: { Self.keepsPageRow($0, fence: fence) })) }
            .filter { !$0.value.isEmpty }
        calendar = calendar.filter { card in !(card.thread_id.map { fence.contains($0) } ?? false) }
        shipments = shipments.filter { card in !(card.thread_id.map { fence.contains($0) } ?? false) }
        if standing.count != before {
            fyeHeadline = nil
            fyeHeadlineKey = nil
        }
    }

    // MARK: - what a save writes

    enum Keeping: Equatable {
        case write(PersistedBoard)
        /// Nothing worth keeping yet; the file is left as it is.
        case nothing
        /// Nothing honest to paint: the file goes.
        case discard
    }

    /// What a save does with `live`. Nothing until the daemon has answered for
    /// the bands this session; then the bands, the zone cards as they stand
    /// (this session's answer, or the rows painted from this same file, which
    /// a save before the zone pass must not wipe), the pages loaded since (a
    /// page painted from disk and never visited again is not restamped), and
    /// the headline only if it was written for the band as it stands (one
    /// written for an older band may be about an auth row that has since left
    /// it). DISCARD when every row of a band that has rows is auth mail:
    /// painted without them it would read as all clear.
    static func keeping(_ live: LiveBoard) -> Keeping {
        guard live.answered.bands else { return .nothing }
        let current = live.fyeHeadlineKey == live.headlineKey
        let board = PersistedBoard(
            standing: live.standing, totalCount: live.totalCount,
            fyeHeadline: current ? live.fyeHeadline : nil,
            fyeHeadlineKey: current ? live.fyeHeadlineKey : nil,
            calendar: live.calendar, shipments: live.shipments,
            mailPages: live.mailPages.filter { live.answered.pages.contains($0.key) },
            mailKeyset: live.mailKeyset, authThreads: live.authThreads,
            lastRefresh: live.lastRefresh)
        if board.standing.isEmpty, !live.standing.isEmpty { return .discard }
        return .write(board)
    }

    /// Whether this board must be written over `last`, what the file holds
    /// from this session (nil: nothing known to be there): when anything but
    /// the date differs, when the file's date is due again (`restampDue`), or
    /// when `forced`.
    func needsWrite(over last: PersistedBoard?, forced: Bool) -> Bool {
        guard !forced, let last else { return true }
        var undated = self
        undated.lastRefresh = last.lastRefresh
        return undated != last || Self.restampDue(lastRefresh, written: last.lastRefresh)
    }

    /// How far the file's date may drift from the daemon's last answer before
    /// unchanged rows are written again for it.
    static let restampAfter: TimeInterval = 600

    /// Whether a file dated `written` is due a new date at `lastRefresh`:
    /// `restampAfter` apart EITHER way, because a clock that stepped back
    /// leaves the file dated ahead of every later answer, and a date more than
    /// `clockSlack` ahead is one the next launch will not paint.
    static func restampDue(_ lastRefresh: Date, written: Date) -> Bool {
        abs(lastRefresh.timeIntervalSince(written)) >= restampAfter
    }

    // MARK: - bytes

    static func encode(_ board: PersistedBoard) throws -> Data {
        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .secondsSince1970
        return try encoder.encode(board)
    }

    /// The board in `data`, if it may be painted at `now`. nil for a file that
    /// does not decode, is another version, or is stale. The auth rules run
    /// again on the way back in, so a file written by anything else still
    /// cannot put auth mail on screen from disk.
    static func decode(_ data: Data, now: Date) -> PersistedBoard? {
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .secondsSince1970
        guard var board = try? decoder.decode(PersistedBoard.self, from: data),
            board.isFresh(now: now)
        else { return nil }
        board.withoutAuthMail()
        return board
    }
}

/// The live board a save is cut from (`PersistedBoard.keeping`), and what of it
/// the daemon has answered for this session.
struct LiveBoard: Sendable {
    var standing: [AttentionUpdate]
    var totalCount: Int?
    var fyeHeadline: String?
    var fyeHeadlineKey: String?
    /// The key of the band as it stands now (`AppStore.headlineKey`).
    var headlineKey: String
    var calendar: [CalendarUpdate]
    var shipments: [Shipment]
    /// The head of each page, by `MailMode` raw value.
    var mailPages: [String: [AttentionUpdate]]
    var mailKeyset: Bool
    /// The store's auth fence (`AppStore.authThreadIds`).
    var authThreads: Set<String>
    var lastRefresh: Date
    var answered: Answered

    /// What the daemon has answered for under the live epoch.
    struct Answered: Sendable, Equatable {
        var bands = false
        /// The pages loaded since, by `MailMode` raw value.
        var pages: Set<String> = []
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

    /// Accounts whose last write failed (a full disk, the phone locking), so
    /// the store does not take the file for current and skip the next save.
    private static let unwritten = OSAllocatedUnfairLock(initialState: Set<UUID>())

    static func lastWriteFailed(account id: UUID) -> Bool {
        unwritten.withLock { $0.contains(id) }
    }

    static func directory() -> URL {
        let base =
            FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first
            ?? URL(fileURLWithPath: NSTemporaryDirectory())
        return base.appendingPathComponent("Passband/Boards", isDirectory: true)
    }

    static func fileURL(for id: UUID, in directory: URL) -> URL {
        directory.appendingPathComponent("board-\(id.uuidString).json")
    }

    /// The account a board file belongs to, from its name; nil for anything
    /// else in the directory.
    static func account(ofFileNamed name: String) -> UUID? {
        guard name.hasPrefix("board-"), name.hasSuffix(".json") else { return nil }
        return UUID(uuidString: String(name.dropFirst("board-".count).dropLast(".json".count)))
    }

    /// The board kept for `id`, if there is one that may be painted now. A
    /// file that is there but stale, of another version, or corrupt is
    /// deleted: nothing will ever paint it. One that cannot be read at all
    /// (the phone locked) is left for a later launch. At the launch's own
    /// priority, not the queue's: the first paint is waiting on it.
    static func load(
        account id: UUID, now: Date = Date(), in directory: URL = BoardFile.directory()
    ) async -> PersistedBoard? {
        let url = fileURL(for: id, in: directory)
        return await withCheckedContinuation { continuation in
            queue.async(qos: .userInitiated, flags: .enforceQoS) {
                continuation.resume(returning: read(url, now: now))
            }
        }
    }

    /// Keep `board` for `id`. Encoded and written on the queue, off the main
    /// actor. `wait`: return only once it is on disk (quitting, or the phone
    /// going to the background, where the process may never run again).
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

    /// Delete every board nothing will paint: those of accounts not in
    /// `accounts`, and those written longer ago than a board may be painted
    /// (a board is written after the answer it is dated by, so one written
    /// that long ago is at least that stale). Run at launch, so the board of
    /// an account nobody opens does not outlive its week. Anything else in the
    /// directory goes too: it is the boards' alone, and every write runs on
    /// this queue, so nothing else there is a write in progress (what an
    /// interrupted atomic write leaves behind is mail content too).
    static func sweep(
        keeping accounts: Set<UUID>, now: Date = Date(), in directory: URL = BoardFile.directory()
    ) {
        queue.async {
            let names = (try? FileManager.default.contentsOfDirectory(atPath: directory.path)) ?? []
            for name in names {
                let url = directory.appendingPathComponent(name)
                guard let id = account(ofFileNamed: name) else {
                    remove(url)
                    continue
                }
                let written =
                    (try? FileManager.default.attributesOfItem(atPath: url.path))?[.modificationDate]
                    as? Date
                let stale = written.map { now.timeIntervalSince($0) >= PersistedBoard.maxAge } ?? false
                if stale || !accounts.contains(id) { remove(url) }
            }
        }
    }

    /// Returns once everything queued before it has run: a save that must be
    /// on disk before the caller goes on (quitting, the phone backgrounded).
    /// Never from the queue itself.
    static func flush() {
        queue.sync {}
    }

    /// The same, without blocking. For the tests.
    static func settle() async {
        await withCheckedContinuation { continuation in
            queue.async { continuation.resume() }
        }
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
            try PrivateFile.write(PersistedBoard.encode(board), to: url, in: directory)
            unwritten.withLock { _ = $0.remove(id) }
        } catch {
            unwritten.withLock { _ = $0.insert(id) }
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
/// once, by whichever comes first: the board painted from disk, the first
/// pull to land, or a launch that is not a sample (a gate, the fixture
/// inbox). Its end message says which, so one build measures both kinds of
/// launch (one with no board kept is the network's). The disk end is the
/// paint's assignment, before the shell's first layout; the network end lands
/// on a shell already mounted. So the disk figure leaves out one mount.
@MainActor
enum LaunchSignpost {
    enum Outcome { case disk, network, skipped }

    private static let signposter = OSSignposter(
        subsystem: "app.passband.client", category: .pointsOfInterest)
    private static var interval: OSSignpostIntervalState?

    static var isOpen: Bool { interval != nil }

    static func begin() {
        guard interval == nil else { return }
        interval = signposter.beginInterval("first board")
    }

    static func end(_ outcome: Outcome) {
        guard let state = interval else { return }
        interval = nil
        switch outcome {
        case .disk: signposter.endInterval("first board", state, "from disk")
        case .network: signposter.endInterval("first board", state, "from the network")
        case .skipped: signposter.endInterval("first board", state, "not a sample: a gate or fixtures")
        }
    }
}
