// Quick clear on the Mac (docs/RENOTIFY.md, "Quick clear"): the daemon's
// `quick_clear` frame on the Mac stream, the banner it becomes, the tap that
// opens its queue, and the sprint's rules for what is next.
//
// Pinned here is what would only ever be wrong on screen or in the queue: a
// frame a newer daemon sends that this one drops, copy that argues against the
// tap (a small percent) or misreads its counts, a tap that falls through to
// the reader, and an item handled elsewhere showing up again.

import Foundation

@main
@MainActor
struct QuickClearTests {
    static var failures = 0
    static var checks = 0

    static func expect(_ ok: Bool, _ what: String) {
        checks += 1
        if !ok {
            failures += 1
            print("FAIL: \(what)")
        }
    }

    static let receipt =
        #"{"message_id":4,"thread_id":"t4","est_secs":8,"basis":"smart_clear","kind":"receipt","sender":"Shop <orders@shop.example>","one_line":"Your order shipped"}"#
    static let promo =
        #"{"message_id":3,"thread_id":"t3","est_secs":0,"basis":"smart_clear","kind":"smart_clear","sender":"Deals <deals@shop.example>","one_line":"Sale ended"}"#
    static let note =
        #"{"message_id":5,"thread_id":"t5","est_secs":40,"basis":"dwell","kind":"surfaced","sender":"Sam Lee <sam@acme.example>","one_line":"Can you look at this?"}"#
    static let odd =
        #"{"message_id":6,"thread_id":"t6","est_secs":20,"basis":"crystal_ball","kind":"brand_new_kind","sender":"x@y.example","one_line":"Hi"}"#

    static func frame(_ items: String, extra: String = "") -> String {
        #"{"quick_clear":12,"trigger":"client_online","at":1793210000,"budget_secs":300,"est_secs":284,"fit_count":14,"open_count":23,"smart_clear_count":6\#(extra),"items":[\#(items)]}"#
    }

    static func signal(
        fit: Int?, open: Int?, est: Int? = nil, budget: Int? = 300, smart: Int? = nil, items: Int = 3
    ) -> QuickClearSignal {
        QuickClearSignal(
            quick_clear: 1, budget_secs: budget, est_secs: est, fit_count: fit, open_count: open,
            smart_clear_count: smart,
            items: (1...max(1, items)).map {
                .init(message_id: $0, thread_id: "t\($0)", sender: "s\($0)@x.example", one_line: "Line \($0)")
            })
    }

    static func main() {
        theFrameDecodesLeniently()
        aStaleFrameIsDropped()
        theTitleLeadsWithCounts()
        thePercentOnlyFlatters()
        smallBacklogsAndPlurals()
        noDashesAnywhere()
        theTapOpensTheQueueNeverTheReader()
        theQueueSkipsWhatIsHandled()
        theEndCardAndTheClock()
        print("quick-clear: \(checks) checks, \(failures) failures")
        if failures > 0 { exit(1) }
    }

    static func theFrameDecodesLeniently() {
        let s = QuickClearSignal.parse(frame([receipt, note].joined(separator: ",")))
        expect(s?.quick_clear == 12 && s?.items.count == 2, "the batch and its items decode")
        expect(s?.items[0].basis == .smart_clear && s?.items[1].basis == .dwell, "known bases decode")
        expect(s?.fit == 14 && s?.open == 23 && s?.smartClears == 6 && s?.budget == 300, "counts decode")
        expect(s?.knownTrigger == .client_online, "the trigger is the re-notify set")

        let unknown = QuickClearSignal.parse(frame(odd))
        expect(unknown?.items.first?.basis == .unknown, "an unknown basis reads as unknown, not a dropped frame")
        expect(unknown?.items.first?.kind == "brand_new_kind", "an unknown kind is kept as words")

        // A bare frame: only the batch and one item, missing every optional.
        let bare = QuickClearSignal.parse(#"{"quick_clear":3,"items":[{"message_id":9}]}"#)
        expect(bare != nil, "a frame missing every optional field still works")
        expect(bare?.budget == 300 && bare?.fit == 1 && bare?.open == 1, "defaults: 5 minutes, the queue's own count")
        expect(bare?.items.first?.sender == "" && bare?.items.first?.thread_id == "", "an item with only an id decodes")
        expect(bare?.knownTrigger == nil && bare?.isFresh(now: 1) == true, "no trigger, no date: still shown")

        // One broken item costs that item, not the frame.
        let mixed = QuickClearSignal.parse(frame(#"{"thread_id":"no id"},"# + receipt + #",{"message_id":"four"},{"message_id":0}"#))
        expect(mixed?.items.map(\.message_id) == [4], "undecodable and zero-id items are dropped: \(String(describing: mixed?.items))")

        // The daemon's shape: no `account`, smart_clear rows first at 0 secs.
        let daemon = QuickClearSignal.parse(frame([promo, receipt, note].joined(separator: ",")))
        expect(daemon?.items.map(\.message_id) == [3, 4, 5], "smart_clear rows keep their place at the front")
        expect(daemon?.items[0].isSmartClear == true && daemon?.items[1].isSmartClear == false, "the sweep's rows are known")
        expect(daemon?.items.allSatisfy { $0.account == nil } == true, "no account field is needed")
        for kind in ["glance", "receipt", "delivery", "account_service", "review", "attend", "read", "smart_clear"] {
            let one = QuickClearSignal.parse(frame(#"{"message_id":7,"kind":"\#(kind)"}"#))
            expect(one?.items.first?.kind == kind, "kind \(kind) decodes")
        }
        let rowsOnly = QuickClearSignal.parse(#"{"quick_clear":5,"items":[\#(promo),\#(promo.replacingOccurrences(of: "\"message_id\":3", with: "\"message_id\":2")),\#(receipt)]}"#)
        expect(rowsOnly?.smartClears == 2, "smart_clear rows offer the sweep even without a count: \(String(describing: rowsOnly?.smartClears))")
        expect(QuickClearSignal.parse(frame(promo))?.smartClears == 6, "the daemon's count wins when it is larger")
        let dupe = QuickClearSignal.parse(frame(receipt + "," + receipt))
        expect(dupe?.items.count == 1, "a repeated message keeps its first slot")

        expect(QuickClearSignal.parse(#"{"quick_clear":0,"items":[\#(receipt)]}"#) == nil, "batch 0 is refused")
        expect(QuickClearSignal.parse(#"{"quick_clear":3,"items":[]}"#) == nil, "nothing to clear is no banner")
        expect(
            QuickClearSignal.parse(#"{"quick_clear":3,"smart_clear_count":4,"items":[]}"#) != nil,
            "a sweep with no queue is still an offer")
        expect(QuickClearSignal.parse("not json") == nil, "garbage is no frame")

        // It is not an event, a re-notify or an open_draft, and they are not it.
        expect(
            QuickClearSignal.parse(#"{"renotify":12,"trigger":"client_online","at":1,"items":[]}"#) == nil,
            "a renotify frame is not a quick clear")
        expect(RenotifySignal.parse(frame(receipt)) == nil, "nor is a quick clear a renotify")
        expect(AgentOpenSignal.parse(frame(receipt)) == nil, "nor an open_draft")
        expect(
            (try? JSONDecoder().decode(Event.self, from: Data(frame(receipt).utf8))) == nil,
            "a quick clear frame is not an Event, so it never moves the cursor")

        // Out-of-range budgets are clamped; open is never under fit.
        expect(signal(fit: 3, open: 3, budget: 5).budget == 60, "a budget under a minute reads as one")
        expect(signal(fit: 3, open: 3, budget: 99_999).budget == 3600, "and over an hour as an hour")
        expect(signal(fit: 9, open: 4).open == 9, "open is never fewer than fit")
        expect(signal(fit: nil, open: nil, items: 3).fit == 3, "fit falls back to the queue")
        expect(signal(fit: 3, open: 3, est: nil).estimate == 300, "no estimates at all: the budget")

        // The payload round-trips: a tap after a relaunch has the same queue.
        let full = QuickClearSignal.parse(frame([receipt, note].joined(separator: ",")))!
        let back = QuickClearSignal.parse(full.payload)
        expect(back?.items.map(\.message_id) == [4, 5], "the payload carries the queue in order")
        expect(back?.items[0].account == nil, "the payload drops the daemon's account label")
        expect(back?.fit == 14 && back?.open == 23 && back?.smartClears == 6, "and the counts")
    }

    static func aStaleFrameIsDropped() {
        let s = QuickClearSignal.parse(frame(receipt))!
        expect(s.isFresh(now: 1_793_210_000), "on time")
        expect(s.isFresh(now: 1_793_210_060), "a minute late is still fresh (the open_draft rule)")
        expect(!s.isFresh(now: 1_793_210_061), "past a minute is dropped")
        expect(!s.isFresh(now: 1_793_209_900), "a clock far ahead is dropped too")
    }

    static func theTitleLeadsWithCounts() {
        let big = EventBanner.quickClearCopy(for: signal(fit: 14, open: 23, smart: 6))
        expect(big.title == "5 minutes clears 14 of your 23 emails", "the owner's example: \(big.title)")
        expect(big.body == "That is 61% of what is waiting. First, one press clears 6 expired promos and past events.", "body: \(big.body)")
        expect(!big.sound, "an offer never chimes")
        expect(big.threadIdentifier == EventBanner.quickClearGroup, "its own group")

        expect(
            EventBanner.quickClearTitle(signal(fit: 3, open: 9, budget: 60)) == "1 minute clears 3 of your 9 emails",
            "a one-minute budget is singular")
        expect(
            EventBanner.quickClearTitle(signal(fit: 1, open: 30)) == "5 minutes clears 1 of your 30 emails",
            "one email of many")
        expect(
            EventBanner.quickClearTitle(signal(fit: 2, open: 9, budget: 330)) == "6 minutes clears 2 of your 9 emails",
            "budgets round to the nearest minute")
        let sweepOnly = signal(fit: 0, open: 0, smart: 1)
        expect(
            EventBanner.quickClearTitle(sweepOnly) == "One press clears 1 expired promo or past event",
            "a sweep with no queue: \(EventBanner.quickClearTitle(sweepOnly))")
        expect(EventBanner.quickClearBody(sweepOnly) == "You can undo it.", "and it says the undo")
    }

    static func thePercentOnlyFlatters() {
        expect(EventBanner.quickClearPercent(fit: 14, open: 23) == 61, "61% is said")
        expect(EventBanner.quickClearPercent(fit: 1, open: 2) == 50, "exactly half is said")
        expect(EventBanner.quickClearPercent(fit: 49, open: 100) == nil, "49% is not")
        expect(EventBanner.quickClearPercent(fit: 6, open: 23) == nil, "26% would argue against the tap")
        expect(EventBanner.quickClearPercent(fit: 199, open: 200) == 99, "never rounds up to 100% when some are left")
        expect(EventBanner.quickClearPercent(fit: 6, open: 6) == nil, "everything fits: the small copy says that")
        expect(EventBanner.quickClearPercent(fit: 0, open: 6) == nil, "nothing fits: no percent")
        let low = EventBanner.quickClearCopy(for: signal(fit: 6, open: 23))
        expect(!low.body.contains("%") && low.body == "One at a time, at your pace.", "a small share leaves the percent out: \(low.body)")
    }

    static func smallBacklogsAndPlurals() {
        expect(
            EventBanner.quickClearTitle(signal(fit: 6, open: 6, est: 170)) == "6 quick ones, about 3 minutes",
            "the owner's small example: \(EventBanner.quickClearTitle(signal(fit: 6, open: 6, est: 170)))")
        expect(
            EventBanner.quickClearTitle(signal(fit: 1, open: 1, est: 30, items: 1)) == "1 quick one, under a minute",
            "one, under a minute")
        expect(
            EventBanner.quickClearTitle(signal(fit: 2, open: 2, est: 75)) == "2 quick ones, about 1 minute",
            "about 1 minute is singular")
        expect(
            EventBanner.quickClearTitle(signal(fit: 4, open: nil, est: 200)) == "4 quick ones, about 3 minutes",
            "no open count reads as everything fits")
        expect(
            EventBanner.quickClearTitle(signal(fit: 9, open: 4, est: 200)) == "9 quick ones, about 3 minutes",
            "an open count under fit reads as everything fits")
        // The queue's own estimates when the frame has no total.
        var summed = signal(fit: 2, open: 2, est: nil, items: 2)
        summed.items[0].est_secs = 50
        summed.items[1].est_secs = 70
        expect(summed.estimate == 120 && EventBanner.quickClearTitle(summed) == "2 quick ones, about 2 minutes", "summed estimates")
        expect(EventBanner.smartClearPhrase(1) == "1 expired promo or past event", "one sweep item is singular")
        expect(EventBanner.smartClearPhrase(6) == "6 expired promos and past events", "several are plural")
        let smallSmart = EventBanner.quickClearCopy(for: signal(fit: 3, open: 3, est: 100, smart: 2))
        expect(smallSmart.body == "First, one press clears 2 expired promos and past events.", "small + sweep body: \(smallSmart.body)")
    }

    static func noDashesAnywhere() {
        var lines: [String] = []
        for (fit, open, smart) in [(14, 23, 6), (6, 23, 0), (6, 6, 1), (1, 1, 0), (0, 0, 3), (1, 2, 0)] {
            let c = EventBanner.quickClearCopy(for: signal(fit: fit, open: open, est: 150, smart: smart))
            lines += [c.title, c.body]
        }
        for (c, l, t) in [(0, 0, false), (12, 0, false), (3, 5, true), (3, 1, false), (0, 4, false), (2, 7, true)] {
            lines.append(QuickClearQueue.summary(cleared: c, elapsed: 250, left: l))
            lines.append(QuickClearQueue.summaryNote(cleared: c, left: l, timeUp: t))
        }
        lines += [QuickClearQueue.timeUpLine(300), QuickClearQueue.timeUpLine(60)]
        for line in lines {
            expect(!line.contains("\u{2014}") && !line.contains("\u{2013}"), "no em or en dash: \(line)")
        }
    }

    static func theTapOpensTheQueueNeverTheReader() {
        let account = UUID(uuidString: "11111111-2222-3333-4444-555555555555")!
        let s = QuickClearSignal.parse(frame([receipt, note].joined(separator: ",")))!
        let id = EventBanner.quickClearIdentifier(account: account, batch: s.quick_clear)
        expect(id == "passband.quickclear.\(account.uuidString).12", "account-prefixed batch: \(id)")
        expect(id != EventBanner.renotifyIdentifier(account: account, batch: 12), "never a re-notify's identity")

        let info = EventBanner.quickClearUserInfo(s, account: account)
        expect(info[EventBanner.eventKey] == nil, "no event id, so ToldEvents never swallows it")
        expect(info[EventBanner.messageKey] == nil, "no message id, so nothing falls through to the reader")
        guard case .quickClear(let tap) = EventBanner.tapRoute(info) else {
            expect(false, "a quick clear banner routes as one: \(EventBanner.tapRoute(info))")
            return
        }
        expect(tap.batch == 12 && tap.trigger == .client_online && tap.items == 2, "the tap names the batch: \(tap)")
        expect(QuickClearSignal.parse(tap.payload)?.items.map(\.message_id) == [4, 5], "and carries the queue it promised")

        var strings = info
        strings[EventBanner.quickClearBatchKey] = "12"
        if case .quickClear(let t) = EventBanner.tapRoute(strings) {
            expect(t.batch == 12, "a string batch survives a relaunch")
        } else {
            expect(false, "string ints route as a quick clear tap")
        }
        var lost = info
        lost[EventBanner.quickClearBatchKey] = nil
        expect(EventBanner.tapRoute(lost) == .thread(nil), "a banner that lost its batch opens nothing")
        var noPayload = info
        noPayload[EventBanner.quickClearPayloadKey] = nil
        if case .quickClear(let t) = EventBanner.tapRoute(noPayload) {
            expect(t.payload.isEmpty, "a lost payload still routes, with no queue")
        } else {
            expect(false, "a lost payload still routes as a quick clear")
        }
        // The re-notify route is untouched.
        expect(EventBanner.renotifyTap(info) == nil, "a quick clear is not a re-notify tap")
    }

    static func theQueueSkipsWhatIsHandled() {
        let items = signal(fit: 5, open: 5, items: 5).items
        let all = QuickClearQueue.pending(items, handled: [], skipped: [])
        expect(all.map(\.message_id) == [1, 2, 3, 4, 5], "nothing handled: the whole queue, in order")
        let some = QuickClearQueue.pending(items, handled: [1, 4], skipped: [2])
        expect(some.map(\.message_id) == [3, 5], "handled and passed items are skipped: \(some.map(\.message_id))")
        // An undo takes the id out of handled: the item is back at the front.
        let undone = QuickClearQueue.pending(items, handled: [4], skipped: [2])
        expect(undone.first?.message_id == 1, "an undone item comes back where it was")
        expect(QuickClearQueue.pending(items, handled: Set(1...5), skipped: []).isEmpty, "all handled: empty")

        func row(_ status: AttentionStatus, resolved: String? = nil, remind: String? = nil) -> AttentionUpdate {
            var r = AttentionUpdate(
                id: 1, thread_id: "t1", tier: .signal, importance: 0, sender: "a@x.example", one_line: "",
                reason: "", status: status)
            r.resolved_at = resolved
            r.remind_at = remind
            return r
        }
        expect(!QuickClearQueue.handledElsewhere(nil), "an uncached row is not known to be handled")
        expect(!QuickClearQueue.handledElsewhere(row(.open)), "an open row is not handled")
        expect(!QuickClearQueue.handledElsewhere(row(.new)), "nor a new one")
        expect(QuickClearQueue.handledElsewhere(row(.done)), "a done row is handled")
        expect(QuickClearQueue.handledElsewhere(row(.open, resolved: "2026-11-18T10:00:00Z")), "a resolved (archived) row is handled")
        expect(QuickClearQueue.handledElsewhere(row(.open, remind: "2026-11-19T09:00:00Z")), "a parked row is handled")
        expect(!QuickClearQueue.handledElsewhere(row(.open, resolved: "")), "an empty stamp is no stamp")

        expect(QuickClearQueue.position(total: 14, pending: 14) == 1, "first of 14")
        expect(QuickClearQueue.position(total: 14, pending: 11) == 4, "4 of 14")
        expect(QuickClearQueue.position(total: 14, pending: 0) == 14, "never past the total")
        expect(QuickClearQueue.position(total: 0, pending: 0) == 0, "an empty queue has no place")

        expect(QuickClearQueue.kindLabel("past_event") == "past event", "kind words")
        expect(QuickClearQueue.kindLabel("account_service") == "account service", "the daemon's kinds read as words")
        expect(QuickClearQueue.kindLabel("smart_clear") == "expired", "a passed-over sweep row is tagged as what it is")
        expect(QuickClearQueue.estimateLabel(0) == nil, "a sweep row's 0 secs shows no time")
        expect(QuickClearQueue.kindLabel("unknown") == nil && QuickClearQueue.kindLabel("") == nil, "no tag for nothing")
        expect(QuickClearQueue.estimateLabel(8) == "8 sec" && QuickClearQueue.estimateLabel(130) == "2 min", "estimates")
        expect(QuickClearQueue.estimateLabel(nil) == nil && QuickClearQueue.estimateLabel(0) == nil, "no estimate, no label")
    }

    static func theEndCardAndTheClock() {
        expect(QuickClearQueue.summary(cleared: 12, elapsed: 250, left: 2) == "Cleared 12 in 4 minutes", "the owner's example")
        expect(QuickClearQueue.summary(cleared: 3, elapsed: 40, left: 0) == "Cleared 3 in under a minute", "under a minute")
        expect(QuickClearQueue.summary(cleared: 1, elapsed: 70, left: 0) == "Cleared 1 in 1 minute", "singular minute")
        expect(QuickClearQueue.summary(cleared: 0, elapsed: 70, left: 4) == "Nothing cleared this time", "nothing, gently")
        expect(QuickClearQueue.summary(cleared: 0, elapsed: 1, left: 0) == "All clear already", "handled elsewhere first")
        expect(QuickClearQueue.summaryNote(cleared: 5, left: 0, timeUp: false) == "That was the whole queue. Nice work.", "emptied")
        expect(
            QuickClearQueue.summaryNote(cleared: 5, left: 1, timeUp: true) == "Time is up. 1 is still in the queue if you want to keep going.",
            "time up, singular")
        expect(QuickClearQueue.summaryNote(cleared: 5, left: 3, timeUp: false) == "3 are still in the queue for later.", "closed early")

        expect(QuickClearQueue.outcome(pending: 0, elapsed: 400, budget: 300) == .emptied, "an empty queue is emptied, late or not")
        expect(QuickClearQueue.outcome(pending: 2, elapsed: 300, budget: 300) == .time_up, "the budget spent")
        expect(QuickClearQueue.outcome(pending: 2, elapsed: 120, budget: 300) == .closed, "left early")

        expect(QuickClearQueue.clock(0) == "0:00" && QuickClearQueue.clock(65) == "1:05", "clock faces")
        expect(QuickClearQueue.clock(300) == "5:00" && QuickClearQueue.clock(-3) == "0:00", "and the edges")
        expect(QuickClearQueue.timeUpLine(300) == "5 minutes are up, no rush", "time up line")
        expect(QuickClearQueue.timeUpLine(60) == "1 minute is up, no rush", "singular")
    }
}
