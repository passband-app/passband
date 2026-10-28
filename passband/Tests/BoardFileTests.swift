// The last board on disk (BoardFile.swift): what a launch paints before the
// daemon answers. Three things have to hold. Nothing a sealed or auth message
// says ever reaches the file, whichever way a row arrives at it. Only a board
// this build can paint is painted: a stale one, another version's, a corrupt
// one is not, and is deleted. And the file is kept the way the saved chats
// are: the owner's alone, out of backups, gone when the account is.

import Foundation

@main
@MainActor
struct BoardFileTests {
    static var failures = 0
    static var checks = 0

    static func main() async {
        authRowsNeverReachTheFile()
        aBoardRoundTrips()
        onlyAFreshBoardOfThisVersionIsPainted()
        await theFileIsKeptPrivately()
        await aFileNobodyWillPaintIsDeleted()
        await aForgottenAccountIsNeverWrittenAgain()
        theLaunchIntervalEndsOnce()

        if failures > 0 {
            print("FAILED: \(failures) of \(checks) checks")
            exit(1)
        }
        print("ok: \(checks) checks passed")
    }

    // MARK: - fixtures

    static func row(_ id: Int, auth: Bool = false) -> AttentionUpdate {
        let kinds = auth ? #", "auth_kinds": ["otp"]"# : ""
        let json = """
            {"id": \(id), "thread_id": "t\(id)", "tier": "signal", "importance": 50,
             "sender": "sender\(id)@example.com", "one_line": "line \(id)", "reason": "why \(id)",
             "status": "new", "preview": "preview text \(id)"\(kinds)}
            """
        return try! JSONDecoder().decode(AttentionUpdate.self, from: Data(json.utf8))
    }

    /// Now, to the second: a whole-second date survives the encoder's
    /// seconds-since-1970 exactly, so a board read back compares equal.
    static var wholeNow: Date { Date(timeIntervalSince1970: Date().timeIntervalSince1970.rounded(.down)) }

    static func board(lastRefresh: Date = BoardFileTests.wholeNow, rows: [AttentionUpdate]? = nil) -> PersistedBoard {
        let rows = rows ?? [row(1), row(2)]
        return PersistedBoard(
            standing: rows, new: [], open: [], totalCount: rows.count, stats: nil,
            fyeHeadline: "Two things today", fyeHeadlineKey: "k", calendar: [], shipments: [],
            mailPages: ["inbox": rows], mailKeyset: true, lastRefresh: lastRefresh)
    }

    static func scratch() -> URL {
        FileManager.default.temporaryDirectory
            .appendingPathComponent("board-file-tests-\(UUID().uuidString)", isDirectory: true)
            .appendingPathComponent("Boards", isDirectory: true)
    }

    // MARK: - what is kept

    /// An auth row is taken out of every band and page on the way in, so the
    /// bytes never hold its preview (which can be the code itself); and taken
    /// out again on the way back, so a file written some other way still
    /// cannot paint one.
    static func authRowsNeverReachTheFile() {
        let kept = board(rows: [row(1), row(7, auth: true), row(2)])
        check(kept.standing.map(\.id) == [1, 2], "the auth row is out of the band")
        check(kept.mailPages["inbox"]?.map(\.id) == [1, 2], "and out of the mail page")
        guard let bytes = try? PersistedBoard.encode(kept),
            let text = String(data: bytes, encoding: .utf8)
        else { return fail("a board encodes") }
        check(!text.contains("preview text 7") && !text.contains("otp"), "no trace of it in the bytes")

        // A file that holds one anyway (hand-written, or a future bug).
        guard var object = (try? JSONSerialization.jsonObject(with: bytes)) as? [String: Any],
            let auth = (try? JSONSerialization.jsonObject(
                with: JSONEncoder().encode(row(7, auth: true))))
        else { return fail("the file is a JSON object") }
        object["standing"] = [auth] + ((object["standing"] as? [Any]) ?? [])
        object["open"] = [auth]
        object["mailPages"] = ["inbox": [auth]]
        guard let doctored = try? JSONSerialization.data(withJSONObject: object),
            let back = PersistedBoard.decode(doctored, now: Date())
        else { return fail("the doctored file still reads") }
        check(back.standing.map(\.id) == [1, 2], "an auth row in the file is not painted")
        check(back.open.isEmpty && back.mailPages["inbox"]?.isEmpty == true, "from any band or page")
    }

    static func aBoardRoundTrips() {
        let kept = board(lastRefresh: Date(timeIntervalSince1970: 1_800_000_000))
        guard let bytes = try? PersistedBoard.encode(kept),
            let back = PersistedBoard.decode(bytes, now: Date(timeIntervalSince1970: 1_800_000_060))
        else { return fail("a fresh board reads back") }
        check(back == kept, "every field comes back as it went")
    }

    /// A week by the daemon's last answer, a little slack for a clock that
    /// stepped back, this version only, and bytes that decode.
    static func onlyAFreshBoardOfThisVersionIsPainted() {
        let now = Date()
        func reads(_ board: PersistedBoard) -> Bool {
            guard let bytes = try? PersistedBoard.encode(board) else { return false }
            return PersistedBoard.decode(bytes, now: now) != nil
        }
        check(reads(board(lastRefresh: now.addingTimeInterval(-6 * 86_400))), "six days old paints")
        check(!reads(board(lastRefresh: now.addingTimeInterval(-PersistedBoard.maxAge))), "a week old does not")
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
        LaunchSignpost.end(fromDisk: true)
        check(!LaunchSignpost.isOpen, "the board from disk closes it")
        LaunchSignpost.end(fromDisk: false)
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
