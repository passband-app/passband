// Smart re-notify on the Mac (docs/RENOTIFY.md): the daemon's `renotify`
// frame, decoded beside `open_draft` on the same stream, and the banner it
// becomes.
//
// What is pinned here is what would only ever be wrong on screen: a frame
// that reads as an event (and moves the cursor), a stale one shown after a lid
// opens, a banner that the first banner's identity swallows or replaces, a
// digest that reads as one sender's mail, and a tap that goes anywhere but the
// first message.

import Foundation

@main
@MainActor
struct RenotifyBannerTests {
    static var failures = 0
    static var checks = 0

    static func expect(_ ok: Bool, _ what: String) {
        checks += 1
        if !ok {
            failures += 1
            print("FAIL: \(what)")
        }
    }

    static let utc: Calendar = {
        var c = Calendar(identifier: .gregorian)
        c.timeZone = TimeZone(identifier: "UTC")!
        return c
    }()

    /// 2026-10-28 10:00 UTC, a Wednesday.
    static let now = Date(timeIntervalSince1970: 1_793_181_600)

    static func frame(_ items: String, at: Int = 1_793_181_600, trigger: String = "client_online") -> String {
        #"{"renotify":12,"trigger":"\#(trigger)","at":\#(at),"items":[\#(items)]}"#
    }

    static let bill =
        #"{"event_id":9,"message_id":4,"thread_id":"t4","kind":"deadline","importance":80,"sender":"Comcast <billing@comcast.com>","one_line":"Invoice due","deadline":null,"why":"due_soon","due_at":"2026-10-29T17:00:00Z"}"#
    static let note =
        #"{"event_id":10,"message_id":5,"thread_id":"t5","kind":"surfaced","importance":60,"sender":"Sarah Chen <sarah@acme.com>","one_line":"Can you review the draft?","deadline":null,"why":"unhandled"}"#
    static let never =
        #"{"event_id":null,"message_id":6,"thread_id":"t6","kind":"deadline","importance":55,"sender":"rent@landlord.example","one_line":"Rent","deadline":"2026-10-28T23:00:00Z","why":"due_soon"}"#
    static let old =
        #"{"event_id":11,"message_id":7,"thread_id":"t7","kind":"surfaced","importance":50,"sender":"Sarah Chen <sarah@acme.com>","one_line":"Ping"}"#

    static func main() {
        theFrameDecodesAndIsNeverAnEvent()
        aStaleFrameIsDropped()
        oneMessageReadsLikeItsFirstBanner()
        severalAreOneDigest()
        dueLabelsReadTheReadersCalendar()
        theIdentityIsTheBatchAndNeverAnEvents()
        aTapOpensTheFirstMessageAndNamesTheBatch()
        print("renotify-banner: \(checks) checks, \(failures) failures")
        if failures > 0 { exit(1) }
    }

    static func theFrameDecodesAndIsNeverAnEvent() {
        let raw = frame(bill + "," + never)
        let s = RenotifySignal.parse(raw)
        expect(s?.renotify == 12 && s?.items.count == 2, "the batch and its items decode")
        expect(s?.knownTrigger == .client_online, "the trigger is known")
        expect(s?.items[1].event_id == nil, "deadline mail that never notified has no event")
        expect(s?.items[0].isDueSoon == true, "why decodes")
        expect(RenotifySignal.parse(frame(old))?.items[0].why == nil, "a frame from before `why` still decodes")
        expect(
            RenotifySignal.parse(frame(note.replacingOccurrences(of: "\"unhandled\"", with: "\"later\"")))?
                .items[0].why == .unhandled,
            "an unknown why reads as unhandled")
        expect(RenotifySignal.parse(frame(bill, trigger: "lunch"))?.knownTrigger == nil, "an unknown trigger still shows, unnamed")
        expect(
            (try? JSONDecoder().decode(Event.self, from: Data(raw.utf8))) == nil,
            "a renotify frame is not an Event, so it never moves the cursor")
        expect(RenotifySignal.parse(#"{"open_draft":3,"at":1}"#) == nil, "an open_draft frame is not a renotify")
        expect(AgentOpenSignal.parse(raw) == nil, "nor is a renotify an open_draft")
        expect(RenotifySignal.parse(frame("")) == nil, "a banner with no items is no banner")
        expect(RenotifySignal.parse(#"{"renotify":0,"trigger":"x","at":1,"items":[\#(bill)]}"#) == nil, "batch 0 is refused")
    }

    static func aStaleFrameIsDropped() {
        let s = RenotifySignal.parse(frame(bill, at: 1000))!
        expect(s.isFresh(now: 1000 + AgentOpenSignal.freshFor), "inside open_draft's window")
        expect(!s.isFresh(now: 1001 + AgentOpenSignal.freshFor), "outside it: a lid opened late")
        expect(s.isFresh(now: 1000 - AgentOpenSignal.freshFor), "clock skew either way")
    }

    static func oneMessageReadsLikeItsFirstBanner() {
        let waiting = EventBanner.renotifyCopy(for: RenotifySignal.parse(frame(note))!, now: now, calendar: utc)
        let first = EventBanner.copy(
            for: try! JSONDecoder().decode(Event.self, from: Data(#"""
            {"id":10,"message_id":5,"thread_id":"t5","kind":"surfaced","tier":"signal","importance":60,"sender":"Sarah Chen <sarah@acme.com>","one_line":"Can you review the draft?","created_at":"2026-10-28T08:00:00Z"}
            """#.utf8)))
        expect(waiting.title == first.title, "the title is the first banner's: \(waiting.title)")
        expect(waiting.body == "Still waiting: Can you review the draft?", "still waiting: \(waiting.body)")
        expect(!waiting.sound, "a nudge, not an alarm")

        let due = EventBanner.renotifyCopy(for: RenotifySignal.parse(frame(bill))!, now: now, calendar: utc)
        expect(due.body == "Due tomorrow: Invoice due", "due_at words it: \(due.body)")
        expect(
            RenotifySignal.parse(frame(never))?.items[0].dueDate == "2026-10-28T23:00:00Z",
            "without due_at the snapshot is the date")
        expect(due.sound, "a deadline chimes")
        expect(due.threadIdentifier == EventBanner.renotifyGroup, "its own group, not the thread's")
    }

    static func severalAreOneDigest() {
        let three = RenotifySignal.parse(frame([note, old, never].joined(separator: ",")))!
        let c = EventBanner.renotifyCopy(for: three, now: now, calendar: utc)
        expect(c.title == "3 emails still need you", "the count: \(c.title)")
        expect(c.body.hasPrefix("Due today: "), "a deadline among them leads: \(c.body)")
        expect(c.body.contains("Sarah Chen") && c.body.contains("and 1 more"), "a couple of senders, once each: \(c.body)")
        expect(c.sound, "a deadline among them chimes")

        let two = RenotifySignal.parse(frame(note + "," + old))!
        let quiet = EventBanner.renotifyCopy(for: two, now: now, calendar: utc)
        expect(quiet.title == "2 emails still need you", "two")
        expect(!quiet.sound, "no deadline, no chime")
        expect(!quiet.body.contains("acme.com"), "names, never addresses: \(quiet.body)")
    }

    static func dueLabelsReadTheReadersCalendar() {
        func label(_ at: String) -> String? { EventBanner.dueLabel(at, now: now, calendar: utc) }
        expect(label("2026-10-28T23:00:00Z") == "Due today", "today")
        expect(label("2026-10-29T00:30:00Z") == "Due tomorrow", "just past midnight is tomorrow")
        expect(label("2026-10-30T17:00:00.123Z") == "Due Friday", "within the week, with a fraction")
        expect(label("2026-11-09T12:00:00Z") == "Due Nov 9", "further out")
        expect(label("2026-10-28T09:00:00Z") == "Past due", "past")
        expect(label("soon") == nil && EventBanner.dueLabel(nil, now: now, calendar: utc) == nil, "unreadable is none")
        var la = Calendar(identifier: .gregorian)
        la.timeZone = TimeZone(identifier: "America/Los_Angeles")!
        // 2026-10-29 03:00 UTC is still the 28th in Los Angeles.
        expect(EventBanner.dueLabel("2026-10-29T03:00:00Z", now: now, calendar: la) == "Due today", "in the reader's zone")
    }

    static func theIdentityIsTheBatchAndNeverAnEvents() {
        let account = UUID(uuidString: "6F9619FF-8B86-D011-B42D-00C04FC964FF")!
        let s = RenotifySignal.parse(frame(bill + "," + note))!
        let id = EventBanner.renotifyIdentifier(account: account, batch: s.renotify)
        expect(id == "passband.renotify.\(account.uuidString).12", "account-prefixed batch: \(id)")
        expect(!id.hasPrefix("passband.event."), "never an event's request id, so neither replaces the other")
        let info = EventBanner.renotifyUserInfo(s, account: account)
        expect(EventBanner.eventIdentity(info) == nil, "no event identity, so ToldEvents cannot swallow it")
        expect(info[EventBanner.accountKey] as? String == account.uuidString, "names its account")
    }

    static func aTapOpensTheFirstMessageAndNamesTheBatch() {
        let account = UUID()
        let s = RenotifySignal.parse(frame(bill + "," + note, trigger: "meeting_end"))!
        let route = EventBanner.tapRoute(EventBanner.renotifyUserInfo(s, account: account))
        expect(
            route
                == .renotify(
                    EventBanner.RenotifyTap(batch: 12, messageId: 4, threadId: "t4", trigger: .meeting_end, items: 2)),
            "the first message, the batch, the trigger: \(route)")
        // Without the batch it is not a renotify tap, and falls back to the message.
        var noBatch = EventBanner.renotifyUserInfo(s, account: account)
        noBatch[EventBanner.renotifyBatchKey] = nil
        expect(EventBanner.tapRoute(noBatch) == .message(4), "no batch: an ordinary message tap")
        // A system that wrote the ints back as strings still routes.
        var strings = EventBanner.renotifyUserInfo(s, account: account)
        strings[EventBanner.renotifyBatchKey] = "12"
        strings[EventBanner.messageKey] = "4"
        if case .renotify(let tap) = EventBanner.tapRoute(strings) {
            expect(tap.batch == 12 && tap.messageId == 4, "string ints")
        } else {
            expect(false, "string ints route as a renotify tap")
        }
    }
}
