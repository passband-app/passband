// MAIL CONTENT AT REST, written one way. The client keeps two files that hold
// mail content (docs/SECURITY.md §6): the saved chats behind search-history
// questions (QuestionChats.swift) and the last board a launch paints
// (BoardFile.swift). Both are written here, so a protection added for one is
// had by both.

import Foundation

enum PrivateFile {
    /// Write `data` to `url`, a file directly inside `directory`, the way mail
    /// content at rest is kept: the directory created when missing and
    /// excluded from backup, the write atomic, and on the Mac the owner's
    /// alone (0600 in a 0700 directory), on the phone under complete file
    /// protection, so it is unreadable while the device is locked.
    static func write(_ data: Data, to url: URL, in directory: URL) throws {
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
    }
}
