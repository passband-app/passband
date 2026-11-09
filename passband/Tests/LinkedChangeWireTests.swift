// Event identity on the wire (Model/WireTypes.swift): another sender's mail
// about the user's event is a `linked` source on their ghost, and when it says
// something different, a `pending_change` on the ghost (or on the real event
// the ghost is drawn as). Every new key is optional and lenient: an older
// daemon sends none of them, and a newer one with a kind this client does not
// know, or a malformed offer, must cost the offer and never the card.

import Foundation

@main
struct LinkedChangeWireTests {
    nonisolated(unsafe) static var failures = 0
    nonisolated(unsafe) static var checks = 0

    static func check(_ ok: Bool, _ what: String) {
        checks += 1
        if !ok {
            failures += 1
            print("FAIL: \(what)")
        }
    }

    static func main() {
        anOlderGhostReadsAsBefore()
        aLinkedSourceAndAMove()
        aCancellationOnARealEvent()
        anUnknownKindIsNoOffer()
        garbageCostsOnlyTheOffer()
        missingOptionalFieldsAreDefaults()
        theNoticeLine()
        print("linked-change: \(checks - failures)/\(checks) passed")
        exit(failures == 0 ? 0 : 1)
    }

    static func ghostJSON(pending: String? = nil, source: String? = nil) -> String {
        let src = source ?? #"{"message_id": 1, "thread_id": "t1", "role": "created"}"#
        let pc = pending.map { #", "pending_change": \#($0)"# } ?? ""
        return #"""
            {"id": 7, "kind": "ticket", "title": "Polar Inertia", "start": "2026-11-07T21:00:00-08:00",
             "all_day": false, "commitment": "committed", "state": "ghost",
             "sources": [\#(src)]\#(pc)}
            """#
    }

    static func ghost(_ json: String) -> TimelineGhost? {
        try? JSONDecoder().decode(TimelineGhost.self, from: Data(json.utf8))
    }

    static func event(_ json: String) -> TimelineEvent? {
        try? JSONDecoder().decode(TimelineEvent.self, from: Data(json.utf8))
    }

    static func anOlderGhostReadsAsBefore() {
        let g = ghost(ghostJSON())
        check(g != nil, "an older ghost decodes")
        check(g?.pending_change == nil, "no offer")
        check(g?.sources.first?.linked == nil, "linked absent is nil")
        check(g?.sources.first?.verified == nil, "verified absent is nil")
    }

    static func aLinkedSourceAndAMove() {
        let json = ghostJSON(
            pending: #"""
                {"ghost_id": 7, "from": 9, "kind": "moved", "start": "2026-11-07T22:00:00-08:00",
                 "all_day": false, "end": null, "verified": false, "message_id": 12, "thread_id": "t12"}
                """#,
            source: #"""
                {"message_id": 1, "thread_id": "t1", "role": "created"},
                {"message_id": 12, "thread_id": "t12", "role": "updated", "linked": true, "verified": false}
                """#)
        let g = ghost(json)
        check(g?.sources.count == 2, "both sources")
        check(g?.sources.last?.linked == true, "the second is linked")
        check(g?.sources.last?.verified == false, "and unverified")
        let c = g?.pending_change
        check(c?.ghost_id == 7 && c?.from == 9, "the ids")
        check(c?.kind == .moved, "a move")
        check(c?.start == "2026-11-07T22:00:00-08:00", "the start")
        check(c?.end == nil, "no stated end")
        check(c?.verified == false, "unverified offer")
        check(c?.message_id == 12 && c?.thread_id == "t12", "its mail")
        // Round trip, as the disk cache writes it.
        if let g, let data = try? JSONEncoder().encode(g) {
            let back = try? JSONDecoder().decode(TimelineGhost.self, from: data)
            check(back == g, "round trips")
        } else {
            check(false, "encodes")
        }
    }

    static func aCancellationOnARealEvent() {
        let json = #"""
            {"id": "e1", "calendar_id": "primary", "start": "2026-11-07", "all_day": true,
             "pending_change": {"ghost_id": 3, "from": 4, "kind": "cancelled", "start": "2026-11-07",
                                "all_day": true, "verified": true}}
            """#
        let e = event(json)
        check(e != nil, "the event decodes")
        check(e?.pending_change?.kind == .cancelled, "a cancellation")
        check(e?.pending_change?.ghost_id == 3, "posted to the ghost drawn as it")
        let plain = event(#"{"id": "e2", "calendar_id": "primary", "start": "2026-11-07", "all_day": true}"#)
        check(plain != nil && plain?.pending_change == nil, "an older event: no offer")
    }

    static func anUnknownKindIsNoOffer() {
        let g = ghost(
            ghostJSON(pending: #"{"ghost_id": 7, "from": 9, "kind": "renamed", "start": "2026-11-07", "all_day": true}"#))
        check(g != nil, "the ghost survives an unknown kind")
        check(g?.pending_change == nil, "and offers nothing it cannot describe")
        let e = event(
            #"""
            {"id": "e1", "calendar_id": "primary", "start": "2026-11-07", "all_day": true,
             "pending_change": {"ghost_id": 3, "from": 4, "kind": "renamed", "start": "2026-11-07", "all_day": true}}
            """#)
        check(e != nil && e?.pending_change == nil, "nor does the event")
    }

    static func garbageCostsOnlyTheOffer() {
        for junk in [#""yes""#, "42", "[]", #"{"kind": "moved"}"#, #"{"ghost_id": "7", "from": 9, "kind": "moved"}"#] {
            let g = ghost(ghostJSON(pending: junk))
            check(g != nil, "the ghost survives pending_change \(junk)")
            check(g?.pending_change == nil, "as no offer: \(junk)")
            let e = event(
                #"{"id": "e1", "calendar_id": "c", "start": "2026-11-07", "all_day": true, "pending_change": \#(junk)}"#)
            check(e != nil && e?.pending_change == nil, "the event survives \(junk)")
        }
        // A linked / verified of the wrong shape costs only itself.
        let g = ghost(
            ghostJSON(source: #"{"message_id": 1, "thread_id": "t1", "role": "created", "linked": "yes", "verified": 0}"#))
        check(g?.sources.count == 1, "the source survives bad flags")
        check(g?.sources.first?.linked == nil && g?.sources.first?.verified == nil, "as nil")
    }

    static func missingOptionalFieldsAreDefaults() {
        let g = ghost(ghostJSON(pending: #"{"ghost_id": 7, "from": 9, "kind": "cancelled"}"#))
        let c = g?.pending_change
        check(c != nil, "ids and kind are enough")
        check(c?.verified == true, "verified defaults to true")
        check(c?.all_day == false && c?.end == nil && c?.message_id == nil && c?.thread_id == nil, "the rest empty")
    }

    static func theNoticeLine() {
        var cal = Calendar(identifier: .gregorian)
        cal.timeZone = TimeZone(identifier: "America/Los_Angeles")!
        let moved = LinkedChange(
            ghost_id: 7, from: 9, kind: .moved, start: "2026-11-07T21:00:00-08:00", all_day: false)
        check(
            Agenda.changeLine(moved, sender: "Endzeit", zone: nil, calendar: cal) == "Endzeit says Sat Nov 7, 9pm",
            "a move: \(Agenda.changeLine(moved, sender: "Endzeit", zone: nil, calendar: cal))")
        let day = LinkedChange(ghost_id: 7, from: 9, kind: .moved, start: "2026-11-08", all_day: true)
        check(Agenda.changeLine(day, sender: "Endzeit", zone: nil, calendar: cal) == "Endzeit says Sun Nov 8", "all day")
        let off = LinkedChange(ghost_id: 7, from: 9, kind: .cancelled, start: "", all_day: false)
        check(
            Agenda.changeLine(off, sender: "  Endzeit ", zone: nil, calendar: cal) == "Endzeit says it's cancelled",
            "a cancellation")
        check(
            Agenda.changeLine(off, sender: nil, zone: nil, calendar: cal) == "Another sender says it's cancelled",
            "no sender yet")
        let bad = LinkedChange(ghost_id: 7, from: 9, kind: .moved, start: "soon", all_day: false)
        check(
            Agenda.changeLine(bad, sender: "Endzeit", zone: nil, calendar: cal) == "Endzeit says the time changed",
            "an unreadable start invents nothing")
        for line in [moved, day, off, bad].map({ Agenda.changeLine($0, sender: "X", zone: nil, calendar: cal) }) {
            check(!line.contains("\u{2014}") && !line.contains("\u{2013}"), "no dashes: \(line)")
        }
    }
}
