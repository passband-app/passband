// The calendar's disk cache (Model/CalendarDisk.swift): what the Calendar page
// paints before the daemon answers. A feed saved for an account comes back
// whole for that account only, a file that will not read is dropped, a feed a
// week old is not painted, forgetting an account deletes its feed, and the
// launch sweep keeps only the accounts still here. On the Mac the file is the
// owner's alone (SECURITY.md §6).

import Foundation

@main
struct CalendarDiskTests {
    nonisolated(unsafe) static var failures = 0
    nonisolated(unsafe) static var checks = 0

    static func check(_ ok: Bool, _ what: String) {
        checks += 1
        if !ok {
            failures += 1
            print("FAIL: \(what)")
        }
    }

    /// A feed as the daemon serves it: a real event with guests and a ghost.
    static let wire = #"""
    {"ghosts":[{"id":7,"kind":"reservation","title":"Dinner at Nopa",
      "start":"2026-10-29T19:30:00-07:00","all_day":false,"end":null,
      "end_estimated":false,"location":"Nopa","commitment":"committed",
      "state":"ghost","mail_state":"live","verified":true,"likelihood":null,
      "sources":[{"message_id":3,"thread_id":"t3","role":"created"}],
      "matched_event":null,
      "tickets":{"message_id":3,"thread_id":"t3","kind":"reservation","attachment":null}}],
     "events":[{"id":"e1","calendar_id":"primary","calendar_name":"Me",
      "title":"Standup","start":"2026-10-29T09:00:00-07:00",
      "end":"2026-10-29T09:15:00-07:00","all_day":false,"location":null,
      "status":"confirmed","self_response":"accepted","busy":true,
      "organizer":"me@example.com","html_link":null,"color":"#4285f4",
      "editable":true,"description":"daily",
      "guests":[{"email":"sam@x.com","name":"Sam","response":"accepted",
        "organizer":false,"self":false}],
      "guests_truncated":false,"guest_editable":true,
      "tickets":{"message_id":5,"thread_id":"t5","kind":"ticket",
        "attachment":{"id":2,"filename":"tickets.pdf","mime":"application/pdf"}}}],
     "calendars":[{"id":"primary","name":"Me","color":"#4285f4",
      "primary":true,"writable":true,"can_invite":true}],
     "calendar":{"granted":true,"synced_at":"2026-10-28T20:00:00Z",
      "error":null,"truncated":false}}
    """#

    static func main() async {
        let dir = FileManager.default.temporaryDirectory
            .appendingPathComponent("calendar-disk-\(UUID().uuidString)", isDirectory: true)
        defer { try? FileManager.default.removeItem(at: dir) }
        let disk = CalendarDisk(dir: dir)
        let me = UUID()
        let other = UUID()
        let feed = try! JSONDecoder().decode(TimelineFeed.self, from: Data(wire.utf8))
        check(feed.events?.count == 1 && feed.ghosts.count == 1, "the fixture decodes")

        // Nothing saved: nothing read, and no folder made for the asking.
        check(await disk.feed(for: me) == nil, "an account with nothing saved reads nil")

        // Saved: back whole, for that account only.
        await disk.save(feed, for: me)
        let back = await disk.feed(for: me)
        check(back == feed, "a saved feed comes back equal: \(String(describing: back))")
        // The Tickets button's field, which a decoder once dropped without a
        // word: on the wire, and still there after a night on disk.
        check(feed.events?.first?.tickets?.attachment?.filename == "tickets.pdf", "an event's tickets decode")
        check(feed.ghosts.first?.tickets?.label == "Booking", "a ghost's tickets decode")
        check(back?.events?.first?.tickets == feed.events?.first?.tickets, "an event's tickets survive the cache")
        check(back?.ghosts.first?.tickets?.message_id == 3, "a ghost's tickets survive the cache")
        check(await disk.feed(for: other) == nil, "another account does not read it")
        let names = (try? FileManager.default.contentsOfDirectory(atPath: dir.path)) ?? []
        check(names.count == 1 && !names[0].contains(me.uuidString),
              "one file, not named by the account id itself: \(names)")

        // Replaced whole by the next save.
        var less = feed
        less.events = []
        await disk.save(less, for: me)
        check(await disk.feed(for: me) == less, "the next save replaces it")

        // A file that will not read is dropped, not half-read.
        let file = dir.appendingPathComponent(names[0])
        try? Data("{not json".utf8).write(to: file)
        check(await disk.feed(for: me) == nil, "a broken file reads nil")
        check(!FileManager.default.fileExists(atPath: file.path), "and is deleted")
        // An older shape too.
        await disk.save(feed, for: me)
        try? Data(#"{"v":0,"feed":{"ghosts":[]}}"#.utf8).write(to: file)
        check(await disk.feed(for: me) == nil, "another version reads nil")

        // Forget deletes it.
        await disk.save(feed, for: me)
        #if os(macOS)
        let perms = (try? FileManager.default.attributesOfItem(atPath: file.path))?[.posixPermissions] as? Int
        check(perms == 0o600, "the owner's alone: \(String(describing: perms))")
        let dirPerms = (try? FileManager.default.attributesOfItem(atPath: dir.path))?[.posixPermissions] as? Int
        check(dirPerms == 0o700, "in a directory that is too: \(String(describing: dirPerms))")
        #endif
        await disk.forget(me)
        check(await disk.feed(for: me) == nil, "forgotten")
        check(!FileManager.default.fileExists(atPath: file.path), "and the file is gone")

        // A week old: not painted, and deleted by the read that finds it.
        let weekAgo = Date().addingTimeInterval(-CalendarDisk.maxAge - 60)
        await disk.save(feed, for: me, now: weekAgo)
        check(await disk.feed(for: me) == nil, "a week-old feed is not painted")
        check(!FileManager.default.fileExists(atPath: file.path), "and is deleted")
        await disk.save(feed, for: me, now: Date().addingTimeInterval(-CalendarDisk.maxAge + 3600))
        check(await disk.feed(for: me) == feed, "six days and change is still painted")

        // The launch sweep: an account no longer here goes, a stray file goes,
        // the live account's fresh feed stays.
        await disk.save(feed, for: other)
        try? Data("half a write".utf8).write(to: dir.appendingPathComponent(".tmp-123"))
        await disk.sweep(keeping: [me])
        check(await disk.feed(for: me) == feed, "the sweep keeps a live account's feed")
        check(await disk.feed(for: other) == nil, "and deletes a removed account's")
        let left = (try? FileManager.default.contentsOfDirectory(atPath: dir.path)) ?? []
        check(left.count == 1, "and anything else: \(left)")

        if failures > 0 {
            print("calendar-disk: \(failures) of \(checks) checks failed")
            exit(1)
        }
        print("calendar-disk: \(checks) checks, 0 failures")
    }
}
