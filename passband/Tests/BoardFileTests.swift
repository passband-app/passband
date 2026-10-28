// The last board on disk (BoardFile.swift): what a launch paints before the
// daemon answers. Three things have to hold. Nothing auth mail says ever
// reaches the file, by any of the signals for it and whichever way a row
// arrives. Only a board this build can paint is painted: a stale one, another
// version's, a corrupt one is not, and is deleted, as is the board of an
// account no longer here. And the file is kept the way the saved chats are:
// the owner's alone, out of backups, gone when the account is.

import Foundation

@main
@MainActor
struct BoardFileTests {
    static var failures = 0
    static var checks = 0

    static func main() async {
        authMailNeverReachesTheFile()
        aDoctoredFileStillPaintsNoAuthMail()
        aBoardRoundTrips()
        aSaveKeepsOnlyWhatTheDaemonHasAnswered()
        aSaveWritesOnlyWhatChanged()
        onlyAFreshBoardOfThisVersionIsPainted()
        await theFileIsKeptPrivately()
        await aFileNobodyWillPaintIsDeleted()
        await theLaunchSweepDeletesWhatNobodyWillPaint()
        await aForgottenAccountIsNeverWrittenAgain()
        theLaunchIntervalEndsOnce()

        if failures > 0 {
            print("FAILED: \(failures) of \(checks) checks")
            exit(1)
        }
        print("ok: \(checks) checks passed")
    }

    // MARK: - fixtures

    /// A row as the daemon serves it. `auth`: the feed's own decision says
    /// auth mail (`auth_kinds`, which only feed rows carry). `placement`: the
    /// zone the mail pages' rows carry, nil for mail triage has not placed.
    static func row(
        _ id: Int, thread: String? = nil, auth: Bool = false, placement: String? = "noise"
    ) -> AttentionUpdate {
        let kinds = auth ? #", "auth_kinds": ["otp"]"# : ""
        let zone = placement.map { #", "placement": "\#($0)""# } ?? ""
        let json = """
            {"id": \(id), "thread_id": "\(thread ?? "t\(id)")", "tier": "signal", "importance": 50,
             "sender": "sender\(id)@example.com", "one_line": "line \(id)", "reason": "why \(id)",
             "status": "new", "preview": "preview text \(id)"\(kinds)\(zone)}
            """
        return try! JSONDecoder().decode(AttentionUpdate.self, from: Data(json.utf8))
    }

    static func card(_ id: Int, thread: String) -> CalendarUpdate {
        CalendarUpdate(
            id: id, message_id: id, thread_id: thread, kind: .invite, event_title: "event \(id)",
            starts_at: nil, organizer: nil, received_at: "2026-10-01T00:00:00Z")
    }

    /// Now, to the second: a whole-second date survives the encoder's
    /// seconds-since-1970 exactly, so a board read back compares equal and a
    /// boundary sits exactly where it is put.
    static var wholeNow: Date { Date(timeIntervalSince1970: Date().timeIntervalSince1970.rounded(.down)) }

    static func board(
        lastRefresh: Date = BoardFileTests.wholeNow, rows: [AttentionUpdate]? = nil,
        pages: [AttentionUpdate]? = nil, calendar: [CalendarUpdate] = [],
        authThreads: Set<String> = []
    ) -> PersistedBoard {
        let rows = rows ?? [row(1), row(2)]
        return PersistedBoard(
            standing: rows, totalCount: rows.count,
            fyeHeadline: "Two things today", fyeHeadlineKey: "k", calendar: calendar, shipments: [],
            mailPages: ["inbox": pages ?? rows], mailKeyset: true, authThreads: authThreads,
            lastRefresh: lastRefresh)
    }

    static func scratch() -> URL {
        FileManager.default.temporaryDirectory
            .appendingPathComponent("board-file-tests-\(UUID().uuidString)", isDirectory: true)
            .appendingPathComponent("Boards", isDirectory: true)
    }

    // MARK: - what is kept

    /// Every signal for auth mail keeps a row out, and every row of its
    /// thread with it: the feed's decision (a band row), the zone (a page row,
    /// which never carries the decision), the store's own fence (the sealed
    /// list's threads), and a page row triage has not placed yet. A page is
    /// cut at its first such row, never holed, and a page cut to nothing is
    /// not kept. What is left of the bytes holds no trace of any of them.
    static func authMailNeverReachesTheFile() {
        let kept = board(
            rows: [row(1), row(7, auth: true), row(8, thread: "t7"), row(2)],
            pages: [
                row(1),
                row(24, placement: "eyes"),
                row(20, placement: "auth"),  // an OTP in the inbox: no auth_kinds on a page row
                row(25),  // below the cut: kept off too, or the scroll would leave a hole
            ],
            calendar: [card(30, thread: "sealed"), card(31, thread: "t31")],
            authThreads: ["sealed"])
        check(kept.standing.map(\.id) == [1, 2], "an auth band row and its thread are out of the band")
        check(kept.mailPages["inbox"]?.map(\.id) == [1, 24], "a page is cut at its first auth row")
        for (what, top) in [
            ("its thread", row(21, thread: "t20")),
            ("an unplaced row", row(22, placement: nil)),
            ("a fenced thread", row(23, thread: "sealed")),
        ] {
            let cut = board(pages: [top, row(1)], authThreads: ["sealed", "t20"])
            check(cut.mailPages["inbox"] == nil, "a page headed by \(what) is not kept at all")
        }
        check(kept.calendar.map(\.id) == [31], "a card from a fenced thread is out of its zone")
        check(kept.authThreads == ["sealed", "t20", "t7"], "the fence keeps every auth thread, by id")
        check(kept.fyeHeadline == nil && kept.fyeHeadlineKey == nil,
            "the headline goes with the band row it may be about")
        guard let bytes = try? PersistedBoard.encode(kept),
            let text = String(data: bytes, encoding: .utf8)
        else { return fail("a board encodes") }
        for gone in [7, 8, 20, 25] {
            check(!text.contains("preview text \(gone)\""), "no trace of row \(gone) in the bytes")
        }
        check(!text.contains("otp"), "nor of an auth kind")

        let clean = board()
        check(clean.fyeHeadline == "Two things today", "a band with nothing taken out keeps its headline")
    }

    /// A file that holds auth mail anyway (hand-written, or a future bug) is
    /// filtered again on the way back in.
    static func aDoctoredFileStillPaintsNoAuthMail() {
        guard let bytes = try? PersistedBoard.encode(board()),
            var object = (try? JSONSerialization.jsonObject(with: bytes)) as? [String: Any],
            let auth = try? JSONSerialization.jsonObject(with: JSONEncoder().encode(row(7, auth: true))),
            let zoned = try? JSONSerialization.jsonObject(
                with: JSONEncoder().encode(row(20, placement: "auth"))),
            let fenced = try? JSONSerialization.jsonObject(
                with: JSONEncoder().encode(row(23, thread: "sealed")))
        else { return fail("the file is a JSON object") }
        object["standing"] = [auth] + ((object["standing"] as? [Any]) ?? [])
        object["mailPages"] = ["inbox": [zoned, fenced]]
        object["authThreads"] = ["sealed"]
        guard let doctored = try? JSONSerialization.data(withJSONObject: object),
            let back = PersistedBoard.decode(doctored, now: Date())
        else { return fail("the doctored file still reads") }
        check(back.standing.map(\.id) == [1, 2], "an auth row in the file is not painted")
        check(back.mailPages["inbox"] == nil, "from any band or page")
    }

    static func aBoardRoundTrips() {
        let kept = board(
            lastRefresh: Date(timeIntervalSince1970: 1_800_000_000),
            calendar: [card(31, thread: "t31")], authThreads: ["sealed"])
        guard let bytes = try? PersistedBoard.encode(kept),
            let back = PersistedBoard.decode(bytes, now: Date(timeIntervalSince1970: 1_800_000_060))
        else { return fail("a fresh board reads back") }
        check(back == kept, "every field comes back as it went")
    }

    // MARK: - what a save writes

    static func live(
        rows: [AttentionUpdate]? = nil, headlineKey: String = "k",
        answered: LiveBoard.Answered = LiveBoard.Answered(bands: true)
    ) -> LiveBoard {
        LiveBoard(
            standing: rows ?? [row(1), row(2)], totalCount: 2, fyeHeadline: "Two things today",
            fyeHeadlineKey: "k", headlineKey: headlineKey, calendar: [card(31, thread: "t31")],
            shipments: [], mailPages: ["inbox": [row(1)], "noise": [row(3)]], mailKeyset: true,
            authThreads: [], lastRefresh: wholeNow, answered: answered)
    }

    /// Nothing before the bands are answered; then only the parts the daemon
    /// has answered for this session, the headline only for the band it was
    /// written about, and the file discarded when the band is all auth mail.
    static func aSaveKeepsOnlyWhatTheDaemonHasAnswered() {
        check(PersistedBoard.keeping(live(answered: LiveBoard.Answered())) == .nothing,
            "nothing is written before a pull has landed")
        guard case .write(let bands) = PersistedBoard.keeping(live()) else {
            return fail("the answered bands are written")
        }
        check(bands.standing.map(\.id) == [1, 2], "the bands are kept")
        check(bands.calendar.map(\.id) == [31], "and the zone cards as they stand")
        check(bands.mailPages.isEmpty, "a page not loaded since is not kept")
        let loaded = LiveBoard.Answered(bands: true, pages: ["inbox"])
        guard case .write(let full) = PersistedBoard.keeping(live(answered: loaded)) else {
            return fail("an answered board is written")
        }
        check(Array(full.mailPages.keys) == ["inbox"], "a loaded page is, but not the one never loaded")
        check(full.fyeHeadline == "Two things today", "a headline for this band is kept")
        guard case .write(let older) = PersistedBoard.keeping(live(headlineKey: "k2")) else {
            return fail("a board with an older headline is written")
        }
        check(older.fyeHeadline == nil && older.fyeHeadlineKey == nil,
            "a headline written for another band is not")
        check(PersistedBoard.keeping(live(rows: [row(7, auth: true)])) == .discard,
            "a band of nothing but auth mail discards the file")
    }

    /// The same rows under a newer date are not written again until the date
    /// has fallen far enough behind; anything else that differs is.
    static func aSaveWritesOnlyWhatChanged() {
        let last = board(lastRefresh: wholeNow)
        let later = board(lastRefresh: wholeNow.addingTimeInterval(60))
        let much = board(lastRefresh: wholeNow.addingTimeInterval(700))
        let other = board(lastRefresh: wholeNow.addingTimeInterval(60), rows: [row(1)])
        let behind = board(lastRefresh: wholeNow.addingTimeInterval(-700))
        check(!later.needsWrite(over: last, forced: false), "the same rows a minute on are not")
        check(much.needsWrite(over: last, forced: false), "the same rows ten minutes on are")
        check(behind.needsWrite(over: last, forced: false),
            "and ten minutes BEHIND, a file a clock that stepped back dated ahead")
        check(other.needsWrite(over: last, forced: false), "different rows are")
        check(later.needsWrite(over: nil, forced: false), "a file not known to hold it is")
        check(later.needsWrite(over: last, forced: true), "and a quit writes regardless")
    }

    /// Under a week by the daemon's last answer, a little slack for a clock
    /// that stepped back, this version only, and bytes that decode.
    static func onlyAFreshBoardOfThisVersionIsPainted() {
        let now = wholeNow
        func reads(_ board: PersistedBoard) -> Bool {
            guard let bytes = try? PersistedBoard.encode(board) else { return false }
            return PersistedBoard.decode(bytes, now: now) != nil
        }
        let week = PersistedBoard.maxAge
        check(reads(board(lastRefresh: now.addingTimeInterval(-6 * 86_400))), "six days old paints")
        check(reads(board(lastRefresh: now.addingTimeInterval(1 - week))), "a second short of a week paints")
        check(!reads(board(lastRefresh: now.addingTimeInterval(-week))), "a week old does not")
        check(reads(board(lastRefresh: now.addingTimeInterval(1_800))), "half an hour ahead paints")
        check(!reads(board(lastRefresh: now.addingTimeInterval(2 * 3_600))), "two hours ahead does not")
        var other = board()
        other.version = PersistedBoard.currentVersion + 1
        check(!reads(other), "another version does not")
        check(PersistedBoard.decode(Data("not json".utf8), now: now) == nil, "garbage does not")
        let bytes = (try? PersistedBoard.encode(board())) ?? Data()
        check(PersistedBoard.decode(bytes.dropLast(), now: now) == nil, "a truncated file does not")
    }

    // MARK: - the file

    /// Out of backups, and on the Mac the owner's alone: 0600 in a 0700
    /// directory.
    static func theFileIsKeptPrivately() async {
        let dir = scratch()
        defer { try? FileManager.default.removeItem(at: dir.deletingLastPathComponent()) }
        let id = UUID()
        let kept = board()
        BoardFile.save(kept, account: id, wait: true, in: dir)
        let url = BoardFile.fileURL(for: id, in: dir)
        check(FileManager.default.fileExists(atPath: url.path), "the board is written")
        let excluded = try? dir.resourceValues(forKeys: [.isExcludedFromBackupKey]).isExcludedFromBackup
        check(excluded == true, "the directory is excluded from backup")
        #if os(macOS)
            func mode(_ path: String) -> Int? {
                let attributes = try? FileManager.default.attributesOfItem(atPath: path)
                return (attributes?[.posixPermissions] as? NSNumber)?.intValue
            }
            check(mode(url.path) == 0o600, "the file is 0600")
            check(mode(dir.path) == 0o700, "its directory is 0700")
        #endif
        let back = await BoardFile.load(account: id, in: dir)
        check(back == kept, "and reads back")
        check(await BoardFile.load(account: UUID(), in: dir) == nil, "another account has no board")
    }

    static func aFileNobodyWillPaintIsDeleted() async {
        let dir = scratch()
        defer { try? FileManager.default.removeItem(at: dir.deletingLastPathComponent()) }
        let stale = UUID()
        BoardFile.save(board(lastRefresh: Date().addingTimeInterval(-30 * 86_400)), account: stale, wait: true, in: dir)
        check(await BoardFile.load(account: stale, in: dir) == nil, "a stale board is not painted")
        check(!FileManager.default.fileExists(atPath: BoardFile.fileURL(for: stale, in: dir).path),
            "and its file is gone")
        let corrupt = UUID()
        try? Data("{".utf8).write(to: BoardFile.fileURL(for: corrupt, in: dir))
        check(await BoardFile.load(account: corrupt, in: dir) == nil, "a corrupt one is not painted")
        check(!FileManager.default.fileExists(atPath: BoardFile.fileURL(for: corrupt, in: dir).path),
            "and its file is gone")
    }

    /// At launch, the boards of accounts no longer here and boards written
    /// longer ago than any may be painted go, whoever's they are, and so does
    /// anything that is not a board (an interrupted write's leftovers); the
    /// live boards stay.
    static func theLaunchSweepDeletesWhatNobodyWillPaint() async {
        let dir = scratch()
        defer { try? FileManager.default.removeItem(at: dir.deletingLastPathComponent()) }
        let live = UUID()
        let removed = UUID()
        let idle = UUID()
        for id in [live, removed, idle] { BoardFile.save(board(), account: id, wait: true, in: dir) }
        let longAgo = Date().addingTimeInterval(-8 * 86_400)
        try? FileManager.default.setAttributes(
            [.modificationDate: longAgo], ofItemAtPath: BoardFile.fileURL(for: idle, in: dir).path)
        let other = dir.appendingPathComponent("notes.txt")
        try? Data("x".utf8).write(to: other)
        check(BoardFile.account(ofFileNamed: "board-\(live.uuidString).json") == live, "a board names its account")
        check(BoardFile.account(ofFileNamed: "notes.txt") == nil, "nothing else does")

        BoardFile.sweep(keeping: [live, idle], in: dir)
        await BoardFile.settle()
        func exists(_ id: UUID) -> Bool {
            FileManager.default.fileExists(atPath: BoardFile.fileURL(for: id, in: dir).path)
        }
        check(exists(live), "a live account's board stays")
        check(!exists(removed), "a removed account's board goes")
        check(!exists(idle), "a board written over a week ago goes, account or not")
        check(!FileManager.default.fileExists(atPath: other.path), "and so does a file that is not a board")
    }

    /// `forget` (the account removed) deletes the board and refuses every
    /// later write for it; `discard` (the same daemon connected again)
    /// deletes it and lets the next save write a new one.
    static func aForgottenAccountIsNeverWrittenAgain() async {
        let dir = scratch()
        defer { try? FileManager.default.removeItem(at: dir.deletingLastPathComponent()) }
        let gone = UUID()
        BoardFile.save(board(), account: gone, wait: true, in: dir)
        BoardFile.forget(account: gone, in: dir)
        BoardFile.save(board(), account: gone, wait: true, in: dir)
        check(await BoardFile.load(account: gone, in: dir) == nil, "a forgotten account has no board")
        check(!FileManager.default.fileExists(atPath: BoardFile.fileURL(for: gone, in: dir).path),
            "and no file, after a later save")

        let again = UUID()
        BoardFile.save(board(), account: again, wait: true, in: dir)
        BoardFile.discard(account: again, in: dir)
        check(await BoardFile.load(account: again, in: dir) == nil, "a discarded board is gone")
        BoardFile.save(board(), account: again, wait: true, in: dir)
        check(await BoardFile.load(account: again, in: dir) != nil, "and the next save writes a new one")
    }

    // MARK: - the launch measurement

    /// One interval a launch: the board from disk ends it, and the first pull
    /// landing behind that leaves it alone.
    static func theLaunchIntervalEndsOnce() {
        LaunchSignpost.begin()
        check(LaunchSignpost.isOpen, "the store's creation opens the interval")
        LaunchSignpost.end(.disk)
        check(!LaunchSignpost.isOpen, "the board from disk closes it")
        LaunchSignpost.end(.network)
        check(!LaunchSignpost.isOpen, "and the pull behind it finds nothing to close")
    }

    // MARK: - checks

    static func check(_ ok: Bool, _ what: String) {
        checks += 1
        if !ok { fail(what) }
    }

    static func fail(_ what: String) {
        failures += 1
        print("FAIL \(what)")
    }
}
