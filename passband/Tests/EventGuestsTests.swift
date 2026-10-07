// Guests on an event, against fixtures (Lib/EventGuests.swift, and the
// guests half of Lib/EventDraft.swift and Lib/CalendarWrite.swift).
//
// This is the first code in Passband that can make Google email OTHER
// PEOPLE, and that mail cannot be unsent. So each of these is an invitation
// somebody did not mean to send, or a person told about a change nobody
// chose to tell them about: Enter in the title field inviting three people,
// a save that says `notify` because a default did, a guest list the daemon
// held half of sent back as the whole of it. Today is the mock's: Monday
// October 5 2026, 3:40pm, in San Francisco.

import Foundation

@main
struct EventGuestsTests {
    nonisolated(unsafe) static var failures = 0
    nonisolated(unsafe) static var checks = 0

    static let zone = TimeZone(identifier: "America/Los_Angeles")!
    static var cal: Calendar {
        var c = Calendar(identifier: .gregorian)
        c.timeZone = zone
        return c
    }
    static let now = at("2026-10-05T15:40:00-07:00")

    static func main() {
        theLineIsRead()
        whatIsAnAddress()
        problemsWithTheLine()
        whoMayInvite()
        theLineForAnEdit()
        theControlsADraftShows()
        enterDoesNotSend()
        notifyOnlyFromTheLabelledControl()
        thePlan()
        aNewEventToTheWire()
        anEditToTheWire()
        deleting()
        guestsAreToldOnlyFromAnInvitingCalendar()
        enterSavesOnlyFromALine()
        editingStartsFromTheOthers()
        aGuestsLocalTime()
        theDetailList()
        theOneSentenceAboutEmail()
        theWave5WireDecodes()
        thePersonCardsZoneDecodes()

        if failures > 0 {
            print("FAILED: \(failures) of \(checks) checks")
            exit(1)
        }
        print("ok: \(checks) checks passed")
    }

    // MARK: fixtures

    static func at(_ s: String) -> Date { AgendaTime.parse(s, zone: nil, calendar: cal)!.date }

    static func draft(guests: String = "", calendarId: String? = "me", title: String = "Dreamers Dinner") -> EventDraft {
        var d = EventDraft()
        d.title = title
        d.calendarId = calendarId
        d.date = "Thu Oct 8"
        d.start = "7:45pm"
        d.end = "9pm"
        d.guests = guests
        return d
    }

    static func guest(
        _ email: String, _ name: String? = nil, _ response: GuestResponse? = .pending, organizer: Bool = false,
        me: Bool = false
    ) -> TimelineGuest {
        TimelineGuest(email: email, name: name, response: response, organizer: organizer, isSelf: me)
    }

    static func event(
        _ id: String = "ev", calendar: String = "me", guests: [TimelineGuest]? = nil, truncated: Bool? = nil,
        guestEditable: Bool? = true
    ) -> TimelineEvent {
        TimelineEvent(
            id: id, calendar_id: calendar, calendar_name: "Personal", title: "Dreamers Dinner",
            start: "2026-10-08T19:45:00-07:00", end: "2026-10-08T21:00:00-07:00", all_day: false,
            start_timezone: nil, location: nil, status: .confirmed, self_response: nil, busy: true,
            organizer: nil, html_link: nil, color: "#039be5", editable: true, description: nil,
            guests: guests, guests_truncated: truncated, guest_editable: guestEditable)
    }

    static let me = guest("me@passband.test", "Me", .accepted, organizer: true, me: true)
    static let sam = guest("sam@x.test", "Sam Lee", .accepted)
    static let ana = guest("Ana@X.test", nil, .tentative)
    static let kenji = guest("kenji@x.test", "Kenji Sato", .declined)

    static let calendars = [
        TimelineCalendar(id: "me", name: "Personal", color: "#039be5", primary: true, writable: true),
        TimelineCalendar(id: "work", name: "Team", color: "#7986cb", primary: false, writable: true),
        TimelineCalendar(id: "hol", name: "Holidays", color: "#0b8043", primary: false, writable: false),
    ]

    /// A wave 5 daemon's feed: it says so by serving `guest_editable`.
    static func feed(_ events: [TimelineEvent] = [event("other", guestEditable: false)]) -> TimelineFeed {
        TimelineFeed(events: events, calendars: calendars)
    }

    /// An older daemon's: the same calendars, events with none of the keys.
    static let oldFeed = TimelineFeed(events: [event("other", guestEditable: nil)], calendars: calendars)

    // MARK: reading

    static func theLineIsRead() {
        let one = EventGuests.read("sam@x.test")
        expect(one.addresses == ["sam@x.test"] && one.unread.isEmpty, "one address")
        let two = EventGuests.read("sam@x.test, ana@x.test")
        expect(two.addresses == ["sam@x.test", "ana@x.test"], "the field's own join, in the order typed")
        expect(EventGuests.read("").addresses.isEmpty && EventGuests.read(" , ,").addresses.isEmpty, "nothing typed")
        expect(EventGuests.read("Sam Lee <sam@x.test>").addresses == ["sam@x.test"], "a display name is dropped")
        expect(
            EventGuests.read("\"Lee, Sam\" <sam@x.test>, ana@x.test").addresses == ["sam@x.test", "ana@x.test"],
            "a comma inside a quoted name is the name's")
        let dupes = EventGuests.read("Sam@X.test, sam@x.test, SAM@X.TEST, ana@x.test")
        expect(dupes.addresses == ["Sam@X.test", "ana@x.test"], "one per person, case-folded, first spelling kept")
        expect(
            EventGuests.read("sam@x.test\nana@x.test\r\nkenji@x.test").addresses.count == 3,
            "a pasted column of addresses is a list")
        let half = EventGuests.read("sam@x.test, ana")
        expect(half.addresses == ["sam@x.test"] && half.unread == ["ana"], "a half-typed guest is not a guest yet")
        expect(EventGuests.read("sam@x.test, ana@").unread == ["ana@"], "nor is one with no host")
        expect(EventGuests.key("  Sam@X.Test ") == "sam@x.test", "identity is case-folded")
        expect(EventGuests.bare("Sam <  sam@x.test >") == "sam@x.test", "the address between the angles")
    }

    static func whatIsAnAddress() {
        for good in ["sam@x.test", "first.last+tag@sub.example.co.uk", "o'brien@x.test", "a@b.c"] {
            expect(EventGuests.isAddress(good), "\(good) is an address")
        }
        for bad in [
            "", "sam", "sam@", "@x.test", "sam@x", "sam@x.", "sam@.test", "sam@x..test", "sam@@x.test",
            "sam@x@y.test", "sam lee@x.test", "sam@x.test;ana@x.test", "<sam@x.test>", "sam@x.test,",
            "\"sam\"@x.test", "sam@x.test\n", "s\u{00E4}m@x.test", "sam@\u{00FC}ber.test",
        ] {
            expect(!EventGuests.isAddress(bad), "\"\(bad)\" is not an address")
        }
        expect(!EventGuests.isAddress(String(repeating: "a", count: 65) + "@x.test"), "a mailbox past 64 is not one")
        expect(!EventGuests.isAddress("a@" + String(repeating: "b", count: 250) + ".test"), "nor an address past 254")
    }

    static func problemsWithTheLine() {
        let owner = "me@passband.test"
        expect(EventGuests.problem(EventGuests.read("sam@x.test"), owner: owner) == nil, "a guest is fine")
        expect(EventGuests.problem(EventGuests.read(""), owner: owner) == nil, "no guests is fine")
        expect(EventGuests.problem(EventGuests.read("sam@x.test, ana"), owner: owner) == .badGuest, "a half-typed guest blocks")
        expect(
            EventGuests.problem(EventGuests.read("sam@x.test, ME@Passband.test"), owner: owner) == .guestIsYou,
            "the calendar's owner is never a guest, however the address is cased")
        expect(EventGuests.problem(EventGuests.read("sam@x.test"), owner: nil) == nil, "an unknown owner refuses nobody")
        expect(EventGuests.problem(EventGuests.read("sam@x.test"), owner: " ") == nil, "nor a blank one")
        let fifty = (1...50).map { "g\($0)@x.test" }.joined(separator: ", ")
        expect(EventGuests.cap == 50, "the daemon's cap")
        expect(EventGuests.problem(EventGuests.read(fifty), owner: owner) == nil, "fifty guests are allowed")
        expect(
            EventGuests.problem(EventGuests.read(fifty + ", g51@x.test"), owner: owner) == .tooManyGuests,
            "fifty-one are not")
        expect(
            EventGuests.problem(EventGuests.read(fifty + ", G1@X.test"), owner: owner) == nil,
            "a repeat of somebody already there is not a fifty-first")
        expect(
            EventGuests.problem(EventGuests.read(fifty + ", g51@x.test, zed"), owner: owner) == .badGuest,
            "what is not an address is said first")
        for p in [EventDraftProblem.badGuest, .guestIsYou, .tooManyGuests] {
            expect(!p.text.isEmpty && !p.text.contains("\u{2014}"), "\(p) has words, and no em dash")
        }
    }

    // MARK: who may invite

    static func whoMayInvite() {
        expect(EventGuests.daemonInvites(feed()), "a daemon that serves guest_editable can invite")
        expect(!EventGuests.daemonInvites(oldFeed), "an older daemon cannot")
        expect(!EventGuests.daemonInvites(nil) && !EventGuests.daemonInvites(TimelineFeed()), "nor no feed at all")

        expect(EventGuests.access(calendarId: "me", editing: nil, feed: feed()) == .field, "a new event on the user's own calendar")
        expect(
            EventGuests.access(calendarId: "work", editing: nil, feed: feed()) == .closed(.otherCalendar),
            "a new event on a shared calendar: closed, with the reason")
        expect(
            EventGuests.access(calendarId: "hol", editing: nil, feed: feed()) == .closed(.otherCalendar),
            "a calendar that cannot be written to")
        expect(
            EventGuests.access(calendarId: nil, editing: nil, feed: feed()) == .closed(.otherCalendar),
            "no calendar picked")
        expect(EventGuests.access(calendarId: "me", editing: nil, feed: oldFeed) == .none, "an older daemon: no line at all")
        expect(EventGuests.access(calendarId: "me", editing: nil, feed: nil) == .none, "no feed: no line")

        // The daemon's own word, if it ever gives one, outranks the rule.
        var said = calendars
        said[0].can_invite = false
        said[1].can_invite = true
        let saying = TimelineFeed(events: [], calendars: said)
        expect(EventGuests.daemonInvites(saying), "a calendar's can_invite also says the daemon knows guests")
        expect(EventGuests.access(calendarId: "work", editing: nil, feed: saying) == .field, "can_invite true opens a shared calendar")
        expect(
            EventGuests.access(calendarId: "me", editing: nil, feed: saying) == .closed(.otherCalendar),
            "can_invite false closes even the primary")
        // The daemon's shape (wave 5): every calendar says can_invite, and a
        // window with no events in it still offers the line.
        var served = calendars
        served[0].can_invite = true
        served[1].can_invite = false
        served[2].can_invite = false
        let empty = TimelineFeed(events: [], calendars: served)
        expect(EventGuests.access(calendarId: "me", editing: nil, feed: empty) == .field, "an empty month on a wave 5 daemon: the line is there")
        expect(EventGuests.access(calendarId: "work", editing: nil, feed: empty) == .closed(.otherCalendar), "and closed on a calendar that may not invite")
        let decoded = try? JSONDecoder().decode(
            TimelineFeed.self,
            from: Data(#"{"ghosts":[],"events":[],"calendars":[{"id":"p","primary":true,"writable":true,"can_invite":true}]}"#.utf8))
        expect(decoded?.calendars?.first?.can_invite == true, "can_invite decodes")
        var readOnly = calendars
        readOnly[2].can_invite = true
        expect(
            !EventGuests.mayInvite(readOnly[2], feed: TimelineFeed(events: [], calendars: readOnly)),
            "a calendar that is not writable never invites")
        for bar in [GuestBar.otherCalendar, .notOrganizer, .tooMany] {
            expect(!bar.text.isEmpty && !bar.text.contains("\u{2014}"), "\(bar) has a reason, and no em dash")
        }
    }

    static func theLineForAnEdit() {
        let f = feed()
        expect(EventGuests.access(calendarId: "me", editing: event(guests: [me, sam]), feed: f) == .field, "the user's own event")
        expect(
            EventGuests.access(calendarId: "me", editing: event(guestEditable: nil), feed: f) == .none,
            "an event from an older daemon: no line")
        expect(
            EventGuests.access(calendarId: "me", editing: event(guests: [me, sam], truncated: true, guestEditable: false), feed: f)
                == .closed(.tooMany),
            "a list cut short is closed")
        expect(
            EventGuests.access(calendarId: "me", editing: event(guests: [me, sam], truncated: true, guestEditable: true), feed: f)
                == .closed(.tooMany),
            "and stays closed even if the daemon called it editable: a partial list is never sent back")
        expect(
            EventGuests.access(calendarId: "me", editing: event(guests: [sam], guestEditable: false), feed: f)
                == .closed(.notOrganizer),
            "somebody else's guest list")
        expect(
            EventGuests.access(calendarId: "me", editing: event(calendar: "work", guests: [me, sam]), feed: f)
                == .closed(.otherCalendar),
            "the user's event on a shared calendar")
        expect(
            EventGuests.access(calendarId: "me", editing: event(calendar: "work"), feed: f) == .closed(.otherCalendar),
            "an edit follows the event's calendar, not the draft's")
    }

    // MARK: the controls

    static func theControlsADraftShows() {
        let new0 = EventGuests.controls(editing: false, before: 0, after: 0)
        expect(new0.primary == .addEvent && new0.quiet == nil, "a new event with nobody: add event")
        let new3 = EventGuests.controls(editing: false, before: 0, after: 3)
        expect(new3.primary == .addAndInvite(3) && new3.quiet == nil, "a new event with guests has ONE control, and it invites")
        let edit0 = EventGuests.controls(editing: true, before: 0, after: 0)
        expect(edit0.primary == .save && edit0.quiet == nil, "an edit with nobody on either side: save")
        let kept = EventGuests.controls(editing: true, before: 2, after: 2)
        expect(kept.primary == .saveAndTell && kept.quiet == .saveWithoutTelling, "an event that has guests: both controls")
        let added = EventGuests.controls(editing: true, before: 0, after: 1)
        expect(added.primary == .saveAndTell && added.quiet == .saveWithoutTelling, "guests added by the edit: both")
        let removed = EventGuests.controls(editing: true, before: 2, after: 0)
        expect(
            removed.primary == .saveAndTell && removed.quiet == .saveWithoutTelling,
            "every guest taken off: both, because the ones removed are still told or not")

        expect(SaveControl.addEvent.label == "add event", "label: add event")
        expect(SaveControl.addAndInvite(3).label == "add and invite 3", "label: add and invite 3")
        expect(SaveControl.addAndInvite(1).label == "add and invite 1", "label: add and invite 1")
        expect(SaveControl.save.label == "save", "label: save")
        expect(SaveControl.saveAndTell.label == "save and tell guests", "label: save and tell guests")
        expect(SaveControl.saveWithoutTelling.label == "save without telling", "label: save without telling")

        expect(SaveControl.addAndInvite(1).notifies && SaveControl.saveAndTell.notifies, "the two that say so notify")
        expect(
            !SaveControl.addEvent.notifies && !SaveControl.save.notifies && !SaveControl.saveWithoutTelling.notifies,
            "the three that do not say so do not")
        // Every control that notifies says so in its own label.
        for c in [SaveControl.addEvent, .addAndInvite(2), .save, .saveAndTell, .saveWithoutTelling] {
            let says = c.label.contains("invite") || c.label.contains("and tell")
            expect(c.notifies == says, "\"\(c.label)\" notifies exactly when its words say it does")
        }
    }

    static func enterDoesNotSend() {
        expect(EventGuests.enterMaySave(EventGuests.controls(editing: false, before: 0, after: 0)), "Enter adds an event with nobody on it")
        expect(EventGuests.enterMaySave(EventGuests.controls(editing: true, before: 0, after: 0)), "Enter saves an edit with nobody on it")
        expect(
            !EventGuests.enterMaySave(EventGuests.controls(editing: false, before: 0, after: 1)),
            "Enter does NOT add an event that would invite one person")
        expect(
            !EventGuests.enterMaySave(EventGuests.controls(editing: false, before: 0, after: 3)),
            "nor three")
        expect(
            !EventGuests.enterMaySave(EventGuests.controls(editing: true, before: 2, after: 2)),
            "Enter does NOT save an edit to an event with guests")
        expect(
            !EventGuests.enterMaySave(EventGuests.controls(editing: true, before: 0, after: 1)),
            "nor one that adds a guest")
        expect(
            !EventGuests.enterMaySave(EventGuests.controls(editing: true, before: 2, after: 0)),
            "nor one that takes every guest off")
        // Whatever the footer, Enter saves only through a control that tells nobody.
        for editing in [false, true] {
            for before in 0...2 {
                for after in 0...2 {
                    let c = EventGuests.controls(editing: editing, before: before, after: after)
                    if EventGuests.enterMaySave(c) {
                        expect(
                            EventGuests.notify(c.primary, offered: c) == false,
                            "where Enter saves (\(editing), \(before), \(after)), nobody is told")
                    }
                }
            }
        }
    }

    static func notifyOnlyFromTheLabelledControl() {
        let new0 = EventGuests.controls(editing: false, before: 0, after: 0)
        let new3 = EventGuests.controls(editing: false, before: 0, after: 3)
        let edit0 = EventGuests.controls(editing: true, before: 0, after: 0)
        let edit2 = EventGuests.controls(editing: true, before: 2, after: 2)

        expect(EventGuests.notify(.addEvent, offered: new0) == false, "add event tells nobody")
        expect(EventGuests.notify(.addAndInvite(3), offered: new3) == true, "add and invite 3 invites")
        expect(EventGuests.notify(.save, offered: edit0) == false, "save tells nobody")
        expect(EventGuests.notify(.saveAndTell, offered: edit2) == true, "save and tell guests tells them")
        expect(EventGuests.notify(.saveWithoutTelling, offered: edit2) == false, "save without telling does not")

        // A control that is not on offer for the draft as it stands saves nothing.
        expect(EventGuests.notify(.addAndInvite(3), offered: new0) == nil, "invite, with no guests on the draft: refused")
        expect(EventGuests.notify(.addAndInvite(2), offered: new3) == nil, "invite 2, when the draft now has 3: refused")
        expect(EventGuests.notify(.addEvent, offered: new3) == nil, "a quiet add, when the draft has guests: refused")
        expect(EventGuests.notify(.saveAndTell, offered: edit0) == nil, "tell, on an event with no guests: refused")
        expect(EventGuests.notify(.save, offered: edit2) == nil, "a plain save, on an event with guests: refused")
        expect(EventGuests.notify(.saveAndTell, offered: new3) == nil, "an edit's control on a new event: refused")

        // Exhaustively: notify is true only for a control whose label says so.
        let every: [SaveControl] = [.addEvent, .addAndInvite(1), .addAndInvite(3), .save, .saveAndTell, .saveWithoutTelling]
        for offered in [new0, new3, edit0, edit2] {
            for asked in every where EventGuests.notify(asked, offered: offered) == true {
                expect(
                    offered.all.contains(asked) && (asked.label.contains("invite") || asked.label.contains("and tell")),
                    "\"\(asked.label)\" is the only kind of control that tells anyone")
            }
        }
    }

    static func thePlan() {
        let f = feed()
        let none = EventGuests.plan(draft(), editing: nil, feed: f)
        expect(none.access == .field && none.after == 0 && none.controls.primary == .addEvent, "a new event, nobody typed")
        let three = EventGuests.plan(draft(guests: "sam@x.test, ana@x.test, kenji@x.test"), editing: nil, feed: f)
        expect(three.after == 3 && three.controls.primary == .addAndInvite(3), "three typed: add and invite 3")
        let dupe = EventGuests.plan(draft(guests: "sam@x.test, SAM@x.test"), editing: nil, feed: f)
        expect(dupe.controls.primary == .addAndInvite(1), "the count is people, not tokens")
        let half = EventGuests.plan(draft(guests: "sam@x.test, an"), editing: nil, feed: f)
        expect(half.controls.primary == .addAndInvite(1), "a half-typed guest is not counted")

        // Guests typed, then a calendar picked that may not invite.
        let shared = EventGuests.plan(draft(guests: "sam@x.test", calendarId: "work"), editing: nil, feed: f)
        expect(
            shared.access == .closed(.otherCalendar) && shared.after == 0 && shared.controls.primary == .addEvent,
            "on a calendar that may not invite, what was typed invites nobody")
        expect(!shared.terms(owner: "me@passband.test").open, "and the body is closed to guests")
        let old = EventGuests.plan(draft(guests: "sam@x.test"), editing: nil, feed: oldFeed)
        expect(old.access == .none && old.after == 0 && old.controls.primary == .addEvent, "an older daemon is sent no guests")
        expect(three.terms(owner: "me@passband.test") == GuestTerms(open: true, owner: "me@passband.test"), "an open line's terms")

        let mine = event(guests: [me, sam, ana])
        let untouched = EventGuests.plan(EventForm.draft(editing: mine, today: now, calendar: cal), editing: mine, feed: f)
        expect(untouched.before == 2 && untouched.after == 2, "the user is not one of their own guests")
        expect(untouched.controls.quiet == .saveWithoutTelling, "an event with guests: two ways to save")
        var cleared = EventForm.draft(editing: mine, today: now, calendar: cal)
        cleared.guests = ""
        let gone = EventGuests.plan(cleared, editing: mine, feed: f)
        expect(gone.before == 2 && gone.after == 0 && gone.controls.primary == .saveAndTell, "everyone taken off: still two")

        let theirs = event(guests: [sam, me], guestEditable: false)
        var typed = EventForm.draft(editing: theirs, today: now, calendar: cal)
        typed.guests = "x@x.test, y@x.test, z@x.test"
        let closed = EventGuests.plan(typed, editing: theirs, feed: f)
        expect(closed.before == 1 && closed.after == 1, "a closed line leaves the guests as they were, whatever it holds")
        expect(closed.controls.primary == .saveAndTell && !closed.terms(owner: nil).open, "closed, with guests: both controls, no guests key")

        let cut = event(guests: [me], truncated: true, guestEditable: false)
        expect(EventGuests.count(cut) == 1, "a list cut short has at least one guest, even with none stored")
        expect(EventGuests.plan(draft(), editing: cut, feed: f).controls.quiet == .saveWithoutTelling, "so it too asks about telling")
        let solo = event(guests: nil)
        expect(EventGuests.plan(draft(), editing: solo, feed: f).controls.primary == .save, "an event with nobody: save")
    }

    // MARK: the wire

    static func keys(_ body: EventWrite) -> [String: Any] {
        (try? JSONSerialization.jsonObject(with: JSONEncoder().encode(body)) as? [String: Any]) ?? [:]
    }

    static func aNewEventToTheWire() {
        let open = GuestTerms(open: true, owner: "me@passband.test")
        let d = draft(guests: "Sam Lee <sam@x.test>, Ana@X.test, sam@x.test")
        guard case .success(let body) = EventForm.wire(d, today: now, calendar: cal, guests: open) else {
            return expect(false, "a draft with guests builds")
        }
        expect(body.guests ?? [] == ["sam@x.test", "Ana@X.test"], "bare addresses, one each")
        expect(body.notify == nil, "the form never says notify")
        let plain = keys(body)
        expect(plain["guests"] as? [String] == ["sam@x.test", "Ana@X.test"] && plain["notify"] == nil, "an unstamped body has no notify key")
        let told = keys(body.stamped(notify: true))
        expect(told["notify"] as? Bool == true, "stamped by a control that invites: notify true")
        let quiet = keys(body.stamped(notify: false))
        expect(quiet["notify"] == nil && quiet["guests"] != nil, "stamped quiet: no notify key at all")

        // Closed (the default): the body names nobody, whatever the line holds.
        guard case .success(let shut) = EventForm.wire(d, today: now, calendar: cal) else {
            return expect(false, "a closed draft builds")
        }
        expect(shut.guests == nil && keys(shut)["guests"] == nil, "closed terms: no guests key")
        expect(keys(shut.stamped(notify: true))["notify"] == nil, "and notify is never written onto a body that names nobody")
        guard case .success(let nobody) = EventForm.wire(draft(), today: now, calendar: cal, guests: open) else {
            return expect(false, "an open draft with nobody builds")
        }
        expect(nobody.guests == nil && keys(nobody.stamped(notify: true))["notify"] == nil, "open, nobody typed: no guests, no notify")

        expect(EventForm.problem(draft(guests: "sam@x.test, an"), today: now, calendar: cal, guests: open) == .badGuest, "a half-typed guest blocks the save")
        expect(
            EventForm.problem(draft(guests: "me@passband.test"), today: now, calendar: cal, guests: open) == .guestIsYou,
            "the user's own address blocks it")
        expect(
            EventForm.problem(draft(guests: "sam@x.test, an"), today: now, calendar: cal) == nil,
            "a closed line is not read, so it cannot block")
        expect(
            EventForm.problem(draft(guests: "sam@x.test", title: ""), today: now, calendar: cal, guests: open) == .noTitle,
            "problems stay in field order: the title first")
        var late = draft(guests: "nope")
        late.notes = String(repeating: "n", count: 2001)
        expect(EventForm.problem(late, today: now, calendar: cal, guests: open) == .badGuest, "guests before notes")
    }

    static func anEditToTheWire() {
        let open = GuestTerms(open: true, owner: "me@passband.test")
        let mine = event(guests: [me, sam, ana])
        let initial = EventForm.draft(editing: mine, today: now, calendar: cal)

        func patch(_ d: EventDraft, _ terms: GuestTerms = open) -> EventPatch? {
            if case .success(let p) = EventForm.patch(d, from: initial, today: now, calendar: cal, guests: terms) { return p }
            return nil
        }
        expect(patch(initial)?.isEmpty == true, "untouched: nothing to send")
        expect(patch(initial)?.json(notify: true).isEmpty == false, "(json writes notify when told to; the store never sends an empty patch)")

        var reordered = initial
        reordered.guests = "ANA@x.test, sam@x.test"
        expect(patch(reordered)?.guests == nil && patch(reordered)?.isEmpty == true, "order and case are not a change")

        var added = initial
        added.guests = initial.guests + ", kenji@x.test"
        expect(patch(added)?.guests ?? [] == ["sam@x.test", "Ana@X.test", "kenji@x.test"], "one added: the WHOLE new list, Google's spellings kept")
        var removed = initial
        removed.guests = "sam@x.test"
        expect(patch(removed)?.guests ?? ["?"] == ["sam@x.test"], "one removed: the whole new list")
        var cleared = initial
        cleared.guests = ""
        expect(patch(cleared)?.guests == [] && patch(cleared)?.isEmpty == false, "everyone removed: an empty list, which is not 'leave alone'")
        expect((patch(cleared)?.json()["guests"] as? [String]) == [], "and it goes as an empty array")

        // Closed: never, whatever the line holds.
        expect(patch(added, .closed)?.guests == nil && patch(added, .closed)?.isEmpty == true, "a closed line never sends guests")
        expect(patch(cleared, .closed)?.guests == nil, "nor an emptied one")

        var titled = initial
        titled.title = "Dreamers Supper"
        let t = patch(titled)
        expect(t?.title == "Dreamers Supper" && t?.guests == nil, "a title edit leaves the guests alone")
        let quiet = t?.json() ?? [:]
        expect(quiet["notify"] == nil && quiet["guests"] == nil, "notify is absent unless asked for")
        expect(t?.json(notify: false)["notify"] == nil, "false writes no key")
        expect(t?.json(notify: true)["notify"] as? Bool == true, "true writes notify: true")

        var bad = initial
        bad.guests = "sam@x.test, an"
        if case .failure(let p) = EventForm.patch(bad, from: initial, today: now, calendar: cal, guests: open) {
            expect(p == .badGuest, "a half-typed guest blocks the edit")
        } else {
            expect(false, "a half-typed guest blocks the edit")
        }
        var own = initial
        own.guests = initial.guests + ", me@passband.test"
        if case .failure(let p) = EventForm.patch(own, from: initial, today: now, calendar: cal, guests: open) {
            expect(p == .guestIsYou, "the user's own address blocks the edit")
        } else {
            expect(false, "the user's own address blocks the edit")
        }
    }

    static func deleting() {
        expect(CalendarWrite.deleteQuery(notify: false).isEmpty, "a quiet delete sends no query")
        expect(CalendarWrite.deleteQuery(notify: true)["notify"] == "true", "a told one says notify=true")

        expect(
            CalendarWrite.failureText(status: 400, .create, guests: true).contains("guests")
                && !CalendarWrite.failureText(status: 400, .create).contains("guests"),
            "a refused write that named guests says to check them, and only then")
        expect(
            CalendarWrite.failureText(status: 400, .edit, guests: true)
                == "Passband could not save that event. Check the times, the calendar and the guests.",
            "the fixed sentence")

        let none = EventGuests.deleteWording(guests: 0, truncated: false)
        expect(none.title == "Delete this event from Google Calendar?" && none.tell == nil && none.quiet == "delete event", "nobody on it: the old question")
        let one = EventGuests.deleteWording(guests: 1, truncated: false)
        expect(one.title == "Delete and tell 1 guest?", "one guest")
        let three = EventGuests.deleteWording(guests: 3, truncated: false)
        expect(three.title == "Delete and tell 3 guests?", "three guests")
        expect(three.tell == "delete and tell guests" && three.quiet == "delete without telling", "both answers, by name")
        let cut = EventGuests.deleteWording(guests: 4, truncated: true)
        expect(cut.title == "Delete and tell the guests?" && cut.tell != nil, "a list cut short does not pretend to a count")
        expect(EventGuests.deleteWording(guests: 0, truncated: true).tell != nil, "even with none stored")
        expect(none.detail == three.detail && !three.detail.isEmpty, "the same second line")

        expect(EventGuests.deleteWording(event(guests: [me, sam, ana]), feed: feed()).title == "Delete and tell 2 guests?", "the user is not counted")
        expect(EventGuests.deleteWording(event(guests: [me]), feed: feed()).tell == nil, "an event with only the user on it has nobody to tell")
        expect(EventGuests.deleteWording(event(guests: nil), feed: feed()).tell == nil, "nor one from an older daemon")
    }

    /// Fix round 1, A2: an update or a cancellation goes out under the
    /// event's calendar's name exactly as an invitation would, so the
    /// controls that tell guests are on offer only where an invitation is.
    /// The daemon refuses `notify` anywhere else with a 400.
    static func guestsAreToldOnlyFromAnInvitingCalendar() {
        let f = feed()
        let mine = event(calendar: "me", guests: [me, sam, ana], guestEditable: true)
        let shared = event(calendar: "work", guests: [me, sam, ana], guestEditable: true)
        expect(EventGuests.mayTell(mine, feed: f), "the user's own calendar may tell")
        expect(!EventGuests.mayTell(shared, feed: f), "a calendar the user only writes to may not")
        var said = calendars
        said[0].can_invite = false
        expect(!EventGuests.mayTell(mine, feed: TimelineFeed(events: [], calendars: said)),
               "the daemon's can_invite false is the last word, even on the primary")
        expect(!EventGuests.mayTell(mine, feed: nil), "no calendars known: nobody is told")

        let own = EventGuests.plan(EventForm.draft(editing: mine, today: now, calendar: cal), editing: mine, feed: f)
        expect(own.controls == SaveControls(primary: .saveAndTell, quiet: .saveWithoutTelling) && !own.silent,
               "own calendar: both controls, as before")
        let plan = EventGuests.plan(EventForm.draft(editing: shared, today: now, calendar: cal), editing: shared, feed: f)
        expect(plan.controls == SaveControls(primary: .saveWithoutTelling), "shared calendar: the silent control alone")
        expect(plan.silent, "and the pane says why")
        expect(EventGuests.notify(.saveAndTell, offered: plan.controls) == nil,
               "a stale \"save and tell guests\" saves nothing at all")
        expect(EventGuests.notify(.saveWithoutTelling, offered: plan.controls) == false, "the silent one tells nobody")
        expect(!plan.controls.all.contains { $0.notifies }, "no control on offer tells anyone")
        let lonely = event(calendar: "work", guests: [me], guestEditable: true)
        expect(
            !EventGuests.plan(EventForm.draft(editing: lonely, today: now, calendar: cal), editing: lonely, feed: f).silent,
            "nobody to tell: nothing to explain")
        expect(!EventGuests.silentLine.contains("\u{2014}") && !EventGuests.silentLine.contains("\n"), "one line, no em dash")

        let ask = EventGuests.deleteWording(shared, feed: f)
        expect(ask.tell == nil && ask.quiet == "delete without telling", "delete on a shared calendar: the silent answer alone")
        expect(ask.detail.contains("not told"), "and it says the guests are not told")
        expect(EventGuests.deleteWording(mine, feed: f).tell == "delete and tell guests", "own calendar: both answers")
    }

    /// Fix round 1, C5: plain ↵ saves only from a single-line field. With
    /// nothing focused (a day clicked on the month beside the pane) it
    /// declines, and the month's own ↵ has it.
    static func enterSavesOnlyFromALine() {
        let quiet = EventGuests.controls(editing: false, before: 0, after: 0)
        let told = EventGuests.controls(editing: false, before: 0, after: 2)
        expect(EventGuests.enter(onLine: true, quiet) == .save, "a line, a save that tells nobody: ↵ saves")
        expect(EventGuests.enter(onLine: false, quiet) == .decline, "no field focused: ↵ passes on")
        expect(EventGuests.enter(onLine: true, told) == .swallow, "a save that would tell: ↵ does nothing")
        expect(EventGuests.enter(onLine: false, told) == .decline, "and with no field focused it still passes on")
    }

    static func editingStartsFromTheOthers() {
        let mine = event(guests: [me, sam, ana])
        expect(EventGuests.fieldText(mine) == "sam@x.test, Ana@X.test", "the line opens with the others, as Google spells them")
        expect(EventForm.draft(editing: mine, today: now, calendar: cal).guests == "sam@x.test, Ana@X.test", "and so does the draft")
        expect(EventForm.draft(editing: event(guests: nil), today: now, calendar: cal).guests == "", "nobody: empty")
        expect(EventGuests.others(mine).map(\.email) == ["sam@x.test", "Ana@X.test"], "others: not the user")
        expect(EventGuests.count(mine) == 2 && EventGuests.count(nil) == 0, "count")
    }

    // MARK: a guest's local time

    static func aGuestsLocalTime() {
        let start = at("2026-10-08T19:45:00-07:00")
        let ny = TimeZone(identifier: "America/New_York")!
        let chicago = TimeZone(identifier: "America/Chicago")!
        let tokyo = TimeZone(identifier: "Asia/Tokyo")!
        let kolkata = TimeZone(identifier: "Asia/Kolkata")!
        let london = TimeZone(identifier: "Europe/London")!
        let seattle = TimeZone(identifier: "America/Vancouver")!

        expect(EventGuests.localTime(start: start, zone: ny, name: "Sam", calendar: cal) == "10:45pm ET for Sam", "New York, the same evening")
        expect(EventGuests.localTime(start: start, zone: chicago, name: "Ana", calendar: cal) == "9:45pm CT for Ana", "Chicago")
        expect(
            EventGuests.localTime(start: start, zone: tokyo, name: "Kenji", calendar: cal) == "11:45am Fri GMT+9 for Kenji",
            "Tokyo: the next day, said")
        expect(
            EventGuests.localTime(start: start, zone: kolkata, name: "Dev", calendar: cal) == "8:15am Fri GMT+5:30 for Dev",
            "a half-hour zone")
        expect(EventGuests.localTime(start: start, zone: zone, name: "Me", calendar: cal) == nil, "the user's own zone: nothing")
        expect(EventGuests.localTime(start: start, zone: seattle, name: "Bo", calendar: cal) == nil, "another zone on the same clock: nothing")
        let morning = at("2026-10-08T10:00:00-07:00")
        expect(EventGuests.localTime(start: morning, zone: ny, name: "Sam", calendar: cal) == "1pm ET for Sam", "on the hour: no minutes")
        expect(
            EventGuests.localTime(start: morning, zone: london, name: "Jo", calendar: cal) == "6pm GMT+1 for Jo",
            "London in October: summer time")
        let winter = at("2026-12-08T10:00:00-08:00")
        expect(
            EventGuests.localTime(start: winter, zone: london, name: "Jo", calendar: cal) == "6pm GMT for Jo",
            "London in December: the label is for the event's date")
        // Every label carries a zone.
        for z in [ny, chicago, tokyo, kolkata, london] {
            let label = EventGuests.zoneLabel(z, at: start)
            expect(!label.isEmpty && !label.contains(" "), "\(z.identifier) has a short zone label (\(label))")
        }
        expect(EventGuests.zoneLabel(TimeZone(identifier: "UTC")!, at: start) != "", "UTC has a label")

        expect(EventGuests.shortName(name: "Sam Lee", email: "sam.lee@x.test") == "Sam", "a first name")
        expect(EventGuests.shortName(name: nil, email: "sam.lee@x.test") == "sam.lee", "no name: the mailbox")
        expect(EventGuests.shortName(name: "  ", email: "sam@x.test") == "sam", "a blank name is no name")
    }

    // MARK: the detail pane

    static func theDetailList() {
        let mine = event(guests: [me, sam, ana, kenji])
        let list = EventGuests.rows(mine)
        expect(list.rows.map(\.name) == ["You", "Sam Lee", "Ana@X.test", "Kenji Sato"], "the user is You; no name falls back to the address")
        expect(list.rows.map(\.response) == ["accepted", "accepted", "maybe", "declined"], "each answer in a word")
        expect(list.rows.map(\.organizer) == [true, false, false, false] && list.more == nil, "the organizer is marked")
        expect(EventGuests.responseWord(.pending) == "no reply" && EventGuests.responseWord(nil) == "no reply", "no answer yet")
        expect(Set(list.rows.map(\.id)).count == 4, "rows have their own ids")

        let many = event(guests: (1...14).map { guest("g\($0)@x.test") })
        expect(EventGuests.rows(many).rows.count == 10 && EventGuests.rows(many).more == "+4 more", "past ten: a count")
        let cut = event(guests: [me, sam], truncated: true, guestEditable: false)
        expect(EventGuests.rows(cut).more == "more guests not shown", "a list the daemon cut short says so")
        let both = event(guests: (1...14).map { guest("g\($0)@x.test") }, truncated: true)
        expect(EventGuests.rows(both).more == "+4 more, and others not shown", "both")
        expect(EventGuests.rows(event(guests: nil)).rows.isEmpty, "nobody: no rows")

        expect(EventGuests.names(mine) == "Sam Lee, Ana@X.test, Kenji Sato", "the closed line names the others")
        expect(EventGuests.names(many) == "g1@x.test, g2@x.test, g3@x.test and 11 more", "and counts past three")
        expect(EventGuests.names(cut) == "Sam Lee and others", "a cut list: and others")
        expect(EventGuests.names(event(guests: [me])) == nil, "only the user: nobody to name")
    }

    static func theOneSentenceAboutEmail() {
        expect(EventGuests.emailLine(editing: false, guests: 0, account: "me@passband.test") == nil, "nobody: no sentence")
        expect(
            EventGuests.emailLine(editing: false, guests: 1, account: "me@passband.test")
                == "Google Calendar will email the invitation from me@passband.test.",
            "one guest")
        expect(
            EventGuests.emailLine(editing: false, guests: 3, account: " me@passband.test ")
                == "Google Calendar will email the invitations from me@passband.test.",
            "three")
        expect(
            EventGuests.emailLine(editing: false, guests: 1, account: nil) == "Google Calendar will email the invitation.",
            "an account not known yet is not guessed")
        expect(
            EventGuests.emailLine(editing: true, guests: 2, account: "me@passband.test")
                == "If you tell them, Google Calendar will email the guests from me@passband.test.",
            "an edit: only if told")
        expect(EventGuests.emailLine(editing: true, guests: 0, account: "me@passband.test") == nil, "an edit left with nobody")
    }

    // MARK: decoding

    static func theWave5WireDecodes() {
        let body = #"""
            {"ghosts": [], "events": [
              {"id": "a", "calendar_id": "primary", "title": "Dinner", "start": "2026-10-08T19:45:00-07:00",
               "all_day": false, "editable": true, "guest_editable": true, "guests_truncated": false,
               "guests": [
                 {"email": "me@passband.test", "name": "Me", "response": "accepted", "organizer": true, "self": true},
                 {"email": "sam@x.test", "name": null, "response": "tentative", "organizer": false, "self": false},
                 {"email": "ana@x.test", "response": "someday"},
                 {"name": "no address"},
                 {"email": "kenji@x.test", "response": 7, "organizer": "yes", "self": []}
               ]},
              {"id": "b", "calendar_id": "primary", "start": "2026-10-09", "all_day": true,
               "guests": "nope", "guests_truncated": "yes", "guest_editable": 1},
              {"id": "c", "calendar_id": "primary", "start": "2026-10-10", "all_day": true},
              {"id": "d", "calendar_id": "primary", "start": "2026-10-11", "all_day": true, "guests": [],
               "guests_truncated": true, "guest_editable": false}
            ]}
            """#
        guard let feed = try? JSONDecoder().decode(TimelineFeed.self, from: Data(body.utf8)) else {
            return expect(false, "the wave 5 timeline decodes")
        }
        let a = feed.events?.first { $0.id == "a" }
        expect(a?.guest_editable == true && a?.guests_truncated == false, "the two flags land")
        expect(a?.guests?.map(\.email) == ["me@passband.test", "sam@x.test", "ana@x.test", "kenji@x.test"], "a guest with no address is dropped, alone")
        expect(a?.guests?.first?.isSelf == true && a?.guests?.first?.organizer == true, "self and organizer")
        expect(a?.guests?[1].response == .tentative && a?.guests?[1].name == nil, "a response, and a null name")
        expect(a?.guests?[2].response == .pending, "an unknown response reads as no reply")
        let k = a?.guests?[3]
        expect(k != nil && k?.response == nil && k?.organizer == nil && k?.isSelf == nil, "malformed guest keys cost only themselves")
        let b = feed.events?.first { $0.id == "b" }
        expect(b != nil && b?.guests == nil && b?.guests_truncated == nil && b?.guest_editable == nil, "malformed event keys are nil, and the event is kept")
        let c = feed.events?.first { $0.id == "c" }
        expect(c != nil && c?.guests == nil && c?.guest_editable == nil, "an older daemon's event: none of it")
        let d = feed.events?.first { $0.id == "d" }
        expect(d?.guests == [] && d?.guests_truncated == true && d?.guest_editable == false, "empty, truncated, closed")
        expect(EventGuests.daemonInvites(feed), "this feed is a wave 5 daemon's")
        let old = try? JSONDecoder().decode(
            TimelineFeed.self,
            from: Data(#"{"ghosts":[],"events":[{"id":"c","calendar_id":"p","start":"2026-10-10","all_day":true}]}"#.utf8))
        expect(old != nil && !EventGuests.daemonInvites(old), "an older feed: no guests line anywhere")
    }

    static func thePersonCardsZoneDecodes() {
        func card(_ extra: String) -> PersonCard? {
            let body = #"{"addr":"sam@x.test","is_self":false,"known_contact":true,"groups":[],"recent_threads":[]"# + extra + "}"
            return try? JSONDecoder().decode(PersonCard.self, from: Data(body.utf8))
        }
        let known = card(#","timezone":{"zone":"America/New_York","source":"offsets","confidence":"high","utc_offset_minutes":-240}"#)
        expect(known?.timezone?.zone == "America/New_York", "the zone's name is read")
        expect(card("") != nil && card("")?.timezone == nil, "an older daemon: no zone, and the card decodes")
        expect(card(#","timezone":null"#) != nil && card(#","timezone":null"#)?.timezone == nil, "null is no zone")
        let odd = card(#","timezone":"America/New_York""#)
        expect(odd != nil && odd?.timezone?.zone == nil, "a shape this client does not know costs the card nothing")
        let numbered = card(#","timezone":{"zone":5}"#)
        expect(numbered != nil && numbered?.timezone?.zone == nil, "nor a zone that is not a string")
    }

    static func expect(_ cond: Bool, _ what: String) {
        checks += 1
        if !cond {
            failures += 1
            print("  FAIL: \(what)")
        }
    }
}
