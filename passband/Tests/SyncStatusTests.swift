// The one derived answer to "is Passband working", and the copy it produces.
//
// The bug this pins: a mailbox Gmail was rate-limiting read as "synced just
// now" and "You're all clear." for days. Each case below is a way the status
// could hide an outage behind a quieter one, or invent an outage that is not
// there, and the copy checks hold the house rules (no em dashes, no raw
// reason codes in a sentence).

import Foundation

@main
@MainActor
struct SyncStatusTests {
    static var failures = 0
    static var checks = 0

    static func main() {
        oldDaemonsReadAsHealthy()
        precedence()
        throttledCopyCarriesRealNumbers()
        triageBlockedStates()
        startupIsNotAnAlarm()
        heroNeverLiesAboutAnEmptyBoard()
        syncedStampIsTheDaemons()
        retriageBlockedDecodes()
        copyRules()

        if failures > 0 {
            print("FAILED: \(failures) of \(checks) checks")
            exit(1)
        }
        print("ok: \(checks) checks passed")
    }

    static let now = Date(timeIntervalSince1970: 1_790_000_000)
    static var utc: Calendar {
        var c = Calendar(identifier: .gregorian)
        c.timeZone = TimeZone(identifier: "UTC")!
        return c
    }
    static let posix = Locale(identifier: "en_US_POSIX")

    static func iso(_ offset: TimeInterval) -> String {
        let f = ISO8601DateFormatter()
        return f.string(from: now.addingTimeInterval(offset))
    }

    static func stats(_ extra: String) -> StoreStats? {
        let json = """
            {"tier_counts":{},"total":0,"sealed":0,\
            "bands":{"standing":0,"new":0,"open":0}\(extra.isEmpty ? "" : ",\(extra)")}
            """
        return try? JSONDecoder().decode(StoreStats.self, from: Data(json.utf8))
    }

    static func derive(_ extra: String, daemonDown: Bool = false) -> SyncStatus {
        SyncStatus.derive(daemonDown: daemonDown, stats: stats(extra), now: now, date: Fmt.date)
    }

    static let throttledSync =
        "\"sync\":{\"ok\":false,\"consecutive_failures\":3,\"error\":{\"kind\":\"quota\",\"reason\":\"RATE_LIMIT_EXCEEDED\"},\"next_retry_at\":\"\(iso(300))\"}"
    static func triage(queued: Int, waiting: Bool = false, extra: String = "") -> String {
        "\"triage\":{\"waiting_for_sync\":\(waiting),\"queued\":\(queued),\"leased\":0,\"budget_parked\":0\(extra)}"
    }

    static func oldDaemonsReadAsHealthy() {
        expect(stats("") != nil, "a stats payload without the new blocks still decodes")
        equal(derive(""), .healthy, "no sync/triage blocks: nothing to report")
        equal(
            SyncStatus.derive(daemonDown: false, stats: nil, now: now, date: Fmt.date), .healthy,
            "no stats yet is not an alarm")
    }

    static func precedence() {
        let all = [
            throttledSync, "\"gmail\":{\"connected\":false}",
            "\"catch_up\":{\"done\":1,\"total\":9}", triage(queued: 5, waiting: true),
        ].joined(separator: ",")
        equal(derive(all, daemonDown: true), .unreachable, "daemon down outranks everything")
        equal(derive(all), .disconnected, "a dead credential outranks throttling")
        let noAuth = [throttledSync, "\"catch_up\":{\"done\":1,\"total\":9}", triage(queued: 5, waiting: true)]
            .joined(separator: ",")
        // THE RECOVERY: the retry is re-walking the mailbox and Gmail is
        // answering it. The error is from the lifecycle before; the catch-up
        // is what is happening now.
        equal(
            derive(noAuth), .catchingUp(done: 1, total: 9),
            "a catch-up in progress outranks the error it is recovering from")
        if case .throttled(_, let reason, let waiting) = derive(
            throttledSync + "," + triage(queued: 5, waiting: true))
        {
            equal(reason, "RATE_LIMIT_EXCEEDED", "the reason rides along")
            equal(waiting, 5, "with the queue size")
        } else {
            expect(false, "throttling outranks triage")
        }
        let network =
            "\"sync\":{\"ok\":false,\"error\":{\"kind\":\"network\",\"reason\":\"no_response\"}}"
        if case .gmailError(let kind, _, let next, _) = derive(network) {
            equal(kind, "network", "other errors keep their kind")
            expect(next == nil, "no retry time when the daemon gave none")
        } else {
            expect(false, "a gmail error is reported")
        }
        equal(
            derive("\"catch_up\":{\"done\":1,\"total\":9}," + triage(queued: 5, waiting: true)),
            .catchingUp(done: 1, total: 9), "catch-up outranks triage waiting")
        // An error while ok is true is not an outage (the daemon omits it, but
        // a client must not trust that alone).
        equal(
            derive("\"sync\":{\"ok\":true,\"error\":{\"kind\":\"quota\",\"reason\":\"x\"}}"),
            .healthy, "a healthy poll with a stale error is healthy")
    }

    static func throttledCopyCarriesRealNumbers() {
        let s = derive(throttledSync + "," + triage(queued: 220, waiting: true))
        let detail = s.detail(now: now, calendar: utc, locale: posix)
        expect(detail.contains("Next try at "), "says when: \(detail)")
        expect(detail.contains("220 emails are waiting for triage."), "says how many: \(detail)")
        expect(!detail.contains("RATE_LIMIT"), "no raw reason in the sentence")
        equal(s.rawReason, "RATE_LIMIT_EXCEEDED", "the code is available for hover")
        expect(s.showsBanner && s.isWarning, "throttling earns a warning banner")
        equal(s.headline, "Gmail is rate-limiting this mailbox", "headline")

        let past = derive(
            "\"sync\":{\"ok\":false,\"error\":{\"kind\":\"quota\",\"reason\":\"x\"},\"next_retry_at\":\"\(iso(-60))\"}"
        )
        expect(
            past.detail(now: now, calendar: utc, locale: posix).contains("Trying again now."),
            "a retry time already passed reads as now")
        let tomorrow = SyncStatus.when(
            now.addingTimeInterval(86_400), now: now, calendar: utc, locale: posix)
        expect(tomorrow?.hasPrefix("tomorrow at ") == true, "tomorrow: \(tomorrow ?? "nil")")
    }

    static func triageBlockedStates() {
        equal(
            derive(triage(queued: 12, waiting: true)), .waitingForSync(waiting: 12),
            "no successful poll with work queued")
        let cooling = derive(triage(queued: 4, extra: ",\"cooldown_until\":\"\(iso(600))\""))
        if case .coolingDown(let until, let waiting) = cooling {
            expect(until != nil, "cooldown carries its end")
            equal(waiting, 4, "and the queue")
        } else {
            expect(false, "a cooldown in force: \(cooling)")
        }
        equal(
            derive(triage(queued: 4, extra: ",\"cooldown_until\":\"\(iso(-600))\"")), .healthy,
            "an expired cooldown is over")
        let budget = derive(
            "\"triage\":{\"waiting_for_sync\":false,\"queued\":9,\"leased\":0,\"budget_parked\":9,\"budget_resumes_at\":\"\(iso(3600))\"}"
        )
        if case .budgetPaused(_, let parked) = budget {
            equal(parked, 9, "parked count")
            expect(
                budget.detail(now: now, calendar: utc, locale: posix).hasPrefix("9 emails will be sorted"),
                "budget copy leads with the number")
            expect(!budget.isWarning, "a spent budget is information, not an alarm")
        } else {
            expect(false, "budget parked: \(budget)")
        }
    }

    static func startupIsNotAnAlarm() {
        // What a daemon before its first poll actually sends: nothing has
        // failed, so sync is ok and triage is not waiting for it.
        let fresh = "\"sync\":{\"ok\":true,\"consecutive_failures\":0},"
        equal(
            derive(fresh + triage(queued: 5)), .healthy,
            "a fresh daemon with work queued is sorting, not blocked")
        equal(
            derive(fresh + "\"catch_up\":{\"done\":40,\"total\":900}," + triage(queued: 5)),
            .catchingUp(done: 40, total: 900), "the first backfill reads as progress")
        // A failed lifecycle holding up nothing is not worth a banner.
        equal(
            derive("\"sync\":{\"ok\":false,\"consecutive_failures\":1}," + triage(queued: 0, waiting: true)),
            .healthy, "waiting for sync with nothing queued blocks nothing")
    }

    static func heroNeverLiesAboutAnEmptyBoard() {
        let waiting = derive(throttledSync + "," + triage(queued: 220, waiting: true))
        let hero = waiting.emptyHero(queued: 220, lastSync: nil, now: now)
        equal(hero?.title, "Nothing sorted yet.", "an unsorted board is not all clear")
        expect(hero?.note.contains("Gmail sync succeeds") == true, "and says what unblocks it")

        let drained = derive(throttledSync + "," + triage(queued: 0))
        let stale = drained.emptyHero(queued: 0, lastSync: now.addingTimeInterval(-7200), now: now)
        expect(stale != nil, "a failing sync never says all clear, even with nothing queued")
        expect(stale?.note.contains("has not synced since") == true, "it says since when")

        expect(
            SyncStatus.healthy.emptyHero(queued: 0, lastSync: now, now: now) == nil,
            "healthy and empty: all clear stands")
        equal(
            SyncStatus.healthy.emptyHero(queued: 3, lastSync: now, now: now)?.title,
            "Sorting your mail.", "healthy with work queued says it is sorting")
    }

    /// Both platforms' "synced" stamp: the daemon's Gmail sync when it says,
    /// never the app's own poll dressed up as one, tinted while sync fails.
    static func syncedStampIsTheDaemons() {
        let stamp = { (extra: String, refresh: Date?) in
            SyncStatus.syncedStamp(
                stats: stats(extra), clientRefresh: refresh, now: now, date: Fmt.date)
        }
        let failing = stamp(
            "\"sync\":{\"ok\":false,\"consecutive_failures\":1,\"last_success_at\":\"\(iso(-7200))\"}",
            now)
        equal(failing.text, "synced 2h ago", "the daemon's stamp, not the app's poll")
        expect(failing.warn, "a stale stamp while sync fails is tinted")
        let never = stamp("\"sync\":{\"ok\":false,\"consecutive_failures\":1}", now)
        equal(never.text, "not synced yet", "no daemon sync is not 'just now'")
        let healthy = stamp(
            "\"sync\":{\"ok\":true,\"consecutive_failures\":0,\"last_success_at\":\"\(iso(-30))\"}",
            nil)
        equal(healthy.text, "synced just now", "healthy")
        expect(!healthy.warn, "and untinted")
        let old = stamp("", now.addingTimeInterval(-300))
        equal(old.text, "synced 5m ago", "an older daemon falls back to the app's poll")
        expect(!old.warn, "with no tint")
    }

    static func retriageBlockedDecodes() {
        let json = """
            {"total":5,"done":1,"started_at":null,"blocked":{"reason":"waiting_for_sync","until":"\(iso(60))"}}
            """
        let p = try? JSONDecoder().decode(RetriageProgress.self, from: Data(json.utf8))
        equal(p?.blocked?.reason, "waiting_for_sync", "blocked decodes")
        let old = try? JSONDecoder().decode(
            RetriageProgress.self, from: Data("{\"total\":1,\"done\":0}".utf8))
        expect(old != nil && old?.blocked == nil, "an old payload has no blocked")
    }

    static func copyRules() {
        let samples: [SyncStatus] = [
            .unreachable, .disconnected,
            .throttled(nextTry: now.addingTimeInterval(300), reason: "RATE_LIMIT_EXCEEDED", waiting: 1),
            .gmailError(kind: "http", reason: "backendError", nextTry: nil, waiting: 0),
            .gmailError(kind: "network", reason: "no_response", nextTry: now, waiting: 2),
            .catchingUp(done: 10, total: 100), .waitingForSync(waiting: 3),
            .coolingDown(until: now.addingTimeInterval(120), waiting: 3),
            .budgetPaused(until: nil, parked: 2),
        ]
        for s in samples {
            let text = [s.headline, s.detail(now: now, calendar: utc, locale: posix), s.oneLine(now: now) ?? ""]
                .joined(separator: " ")
            expect(!text.contains("\u{2014}"), "no em dash: \(text)")
            if let raw = s.rawReason {
                let code = raw.split(separator: " ").last.map(String.init) ?? raw
                expect(!text.contains(code), "no reason code in copy: \(text)")
            }
        }
        equal(SyncStatus.healthy.oneLine(now: now), nil, "healthy has nothing to say")
    }

    static func expect(_ ok: Bool, _ what: String) {
        checks += 1
        if !ok {
            failures += 1
            print("  FAIL: \(what)")
        }
    }

    static func equal<T: Equatable>(_ a: T, _ b: T, _ what: String) {
        checks += 1
        if a != b {
            failures += 1
            print("  FAIL: \(what): \(a) != \(b)")
        }
    }
}
