// Measured email heights kept across launches (Lib/FrameHeightFile.swift).
// What has to hold: a height is only ever answered for the exact document,
// width, type and build it was measured under; the file stays under its cap;
// and it is kept the way the board file is (private, out of backups, gone with
// the account, unreadable files deleted).

import Foundation

@main
@MainActor
struct FrameHeightFileTests {
    static var failures = 0
    static var checks = 0

    static func main() {
        aHeightAnswersOnlyItsOwnQuestion()
        theSignatureIsTheDocument()
        theFileStaysUnderItsCap()
        theFileIsKeptPrivatelyAndRoundTrips()
        whatCannotBeReadIsDeleted()
        forgetAndSweep()

        if failures > 0 {
            print("FAILED: \(failures) of \(checks) checks")
            exit(1)
        }
        print("ok: \(checks) checks passed")
    }

    static func check(_ ok: Bool, _ what: String) {
        checks += 1
        if !ok {
            failures += 1
            print("FAIL: \(what)")
        }
    }

    static func scratch() -> URL {
        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent("frame-heights-\(UUID().uuidString)", isDirectory: true)
        return url
    }

    static func sample() -> FrameHeightFile {
        var file = FrameHeightFile(build: "2200", width: 640, metrics: "14.0/14.0/9.0/12.0")
        file.record("101", height: 812, signature: 7, at: 1)
        file.record("102.chat", height: 120, signature: 9, at: 2)
        return file
    }

    static func aHeightAnswersOnlyItsOwnQuestion() {
        let file = sample()
        check(file.holds(build: "2200", width: 640, metrics: "14.0/14.0/9.0/12.0"), "same conditions: held")
        check(file.holds(build: "2200", width: 640.4, metrics: "14.0/14.0/9.0/12.0"),
            "a width within half a point is the same column")
        check(!file.holds(build: "2201", width: 640, metrics: "14.0/14.0/9.0/12.0"), "another build: not held")
        check(!file.holds(build: "2200", width: 720, metrics: "14.0/14.0/9.0/12.0"), "another width: not held")
        check(!file.holds(build: "2200", width: 640, metrics: "15.0/16.0/10.0/13.0"), "another density: not held")

        check(file.height("101", signature: 7) == 812, "the height for this document")
        check(file.height("101", signature: 8) == nil, "a changed document has no height")
        check(file.height("103", signature: 7) == nil, "an unmeasured key has none")
        var zero = file
        zero.record("104", height: 0, signature: 1, at: 3)
        check(zero.height("104", signature: 1) == nil, "a zero height is never filed")
    }

    static func theSignatureIsTheDocument() {
        let html = "<p>Your statement is ready.</p><img src='https://x/y.png'>"
        let a = FrameHeightFile.signature(html: html, allowRemote: true)
        check(a == FrameHeightFile.signature(html: html, allowRemote: true), "the same document, the same signature")
        check(a != FrameHeightFile.signature(html: html, allowRemote: false),
            "remote images off is another document")
        check(a != FrameHeightFile.signature(html: html + " ", allowRemote: true), "one byte more is another")
        // Pinned, so a change to the hash (which would orphan every file on
        // disk silently) is a decision, not an accident.
        check(FrameHeightFile.signature(html: "", allowRemote: false) == 0xaf63_bf4c_8601_bb45,
            "the signature of an empty document is the pinned value (got \(String(FrameHeightFile.signature(html: "", allowRemote: false), radix: 16)))")
    }

    static func theFileStaysUnderItsCap() {
        var file = FrameHeightFile(build: "1", width: 600, metrics: "m")
        for i in 0..<(FrameHeightFile.capacity + 25) {
            file.record("\(i)", height: 100, signature: 1, at: Double(i))
        }
        check(file.heights.count == FrameHeightFile.capacity, "the file holds no more than its cap")
        check(file.heights["0"] == nil && file.heights["24"] == nil, "the oldest measured go first")
        check(file.heights["25"] != nil && file.heights["\(FrameHeightFile.capacity + 24)"] != nil,
            "and the newest stay")
    }

    static func theFileIsKeptPrivatelyAndRoundTrips() {
        let dir = scratch()
        let account = UUID()
        do {
            try FrameHeightFile.write(sample(), account: account, in: dir)
        } catch {
            return check(false, "the file writes (\(error))")
        }
        check(FrameHeightFile.load(account: account, in: dir) == sample(), "what is written is read back")
        let url = FrameHeightFile.url(account: account, in: dir)
        let fileMode = (try? FileManager.default.attributesOfItem(atPath: url.path)[.posixPermissions]) as? Int
        let dirMode = (try? FileManager.default.attributesOfItem(atPath: dir.path)[.posixPermissions]) as? Int
        check(fileMode == 0o600, "the file is the owner's alone (got \(String(fileMode ?? -1, radix: 8)))")
        check(dirMode == 0o700, "and so is its directory (got \(String(dirMode ?? -1, radix: 8)))")
        let excluded = (try? dir.resourceValues(forKeys: [.isExcludedFromBackupKey]))?.isExcludedFromBackup
        check(excluded == true, "the directory is out of backups")
        check(FrameHeightFile.load(account: UUID(), in: dir) == nil, "another account reads nothing")
        try? FileManager.default.removeItem(at: dir)
    }

    static func whatCannotBeReadIsDeleted() {
        let dir = scratch()
        let account = UUID()
        let url = FrameHeightFile.url(account: account, in: dir)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        try? Data("{not json".utf8).write(to: url)
        check(FrameHeightFile.load(account: account, in: dir) == nil, "garbage is not read")
        check(!FileManager.default.fileExists(atPath: url.path), "and is deleted")

        var other = sample()
        other.version = FrameHeightFile.currentVersion + 1
        try? JSONEncoder().encode(other).write(to: url)
        check(FrameHeightFile.load(account: account, in: dir) == nil, "another version is not read")
        check(!FileManager.default.fileExists(atPath: url.path), "and is deleted")
        try? FileManager.default.removeItem(at: dir)
    }

    static func forgetAndSweep() {
        let dir = scratch()
        let kept = UUID()
        let gone = UUID()
        let removed = UUID()
        for account in [kept, gone, removed] { try? FrameHeightFile.write(sample(), account: account, in: dir) }
        try? Data("stray".utf8).write(to: dir.appendingPathComponent("stray.txt"))

        FrameHeightFile.forget(account: gone, in: dir)
        check(FrameHeightFile.load(account: gone, in: dir) == nil, "a forgotten account's heights are gone")
        check(FrameHeightFile.load(account: kept, in: dir) != nil, "and only that account's")

        FrameHeightFile.sweep(keeping: [kept], in: dir)
        let left = (try? FileManager.default.contentsOfDirectory(atPath: dir.path)) ?? []
        check(left == [FrameHeightFile.url(account: kept, in: dir).lastPathComponent],
            "the sweep leaves only the accounts still here (left \(left))")
        try? FileManager.default.removeItem(at: dir)
    }
}
