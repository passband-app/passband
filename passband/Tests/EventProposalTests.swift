// The assistant's one calendar tool, held to its rule: it PROPOSES, in the
// event pane, and writes nothing (Lib/EventProposal.swift).
//
// Each of these is wrong only once, on somebody's real calendar: the tool
// offered to a mailbox with no Calendar page, a proposal for 4pm landing at
// 11pm because an offset was dropped, an all-day trip a day short, guests on
// a calendar that cannot invite them, a second proposal silently replacing an
// event the user was half way through typing, a malformed proposal opening a
// half-filled pane. Today is the event suite's: Monday October 5 2026, 3:40pm
// in San Francisco.

import Foundation

@main
struct EventProposalTests {
    nonisolated(unsafe) static var failures = 0
    nonisolated(unsafe) static var checks = 0

    static let zone = TimeZone(identifier: "America/Los_Angeles")!
    static var cal: Calendar {
        var c = Calendar(identifier: .gregorian)
        c.timeZone = zone
        return c
    }
    static let now = AgendaTime.parse("2026-10-05T15:40:00-07:00", zone: nil, calendar: cal)!.date

    static func main() {
        theToolIsOfferedOnlyWithTheCalendar()
        readingTheCall()
        aTimedProposalBecomesTheDraft()
        anAllDayProposalBecomesTheDraft()
        theCalendarItGoesOn()
        guestsAreRealGuestsWhereTheyMayBe()
        aBadProposalIsAnErrorNotAHalfFilledPane()
        theAssistantWritesNoNotes()
        theTitleAndThePlaceAreOneShortLine()
        itNeverTakesTheScreenFromSomebody()
        aPaneInUseRefuses()
        aStaleRunDraftsNothing()
        aParkedProposalWaitsForMailBeingWritten()
        whatTheModelIsTold()

        if failures > 0 {
            print("FAILED: \(failures) of \(checks) checks")
            exit(1)
        }
        print("ok: \(checks) checks passed")
    }

    static func expect(_ ok: Bool, _ what: String) {
        checks += 1
        if !ok {
            failures += 1
            print("  FAIL: \(what)")
        }
    }

    // MARK: fixtures

    static func calendarRow(_ id: String, _ name: String, primary: Bool = false, writable: Bool = true, invite: Bool? = nil)
        -> TimelineCalendar
    {
        let json = """
            {"id":"\(id)","name":"\(name)","color":"#039be5","primary":\(primary),"writable":\(writable)\
            \(invite.map { ",\"can_invite\":\($0)" } ?? "")}
            """
        return try! JSONDecoder().decode(TimelineCalendar.self, from: Data(json.utf8))
    }

    /// A wave-5 daemon's feed: a primary that may invite, a second writable
    /// calendar that may not, and one that is read-only.
    static var feed: TimelineFeed {
        var f = TimelineFeed(ghosts: [], events: [])
        f.calendars = [
            calendarRow("me@example.com", "Jamie", primary: true, invite: true),
            calendarRow("family", "Family", invite: false),
            calendarRow("holidays", "Holidays", writable: false),
        ]
        return f
    }

    static func input(_ pairs: [String: JSONValue]) -> [String: JSONValue] { pairs }

    static func proposal(
        _ title: String = "Dinner with Steven", start: String = "2026-10-09T16:00:00", end: String? = nil,
        allDay: Bool = false, calendar: String? = nil, location: String? = nil,
        guests: [String] = []
    ) -> EventProposal {
        EventProposal(
            title: title, start: start, end: end, allDay: allDay, calendar: calendar, location: location,
            guests: guests)
    }

    static func draft(_ p: EventProposal, feed f: TimelineFeed? = feed) -> Result<EventDraft, ProposalRefusal> {
        EventProposals.draft(p, feed: f, owner: "me@example.com", today: now, calendar: cal)
    }

    static func made(_ p: EventProposal) -> EventDraft? {
        if case .success(let d) = draft(p) { return d }
        return nil
    }

    static func refusal(_ p: EventProposal, feed f: TimelineFeed? = feed) -> String? {
        if case .failure(let r) = draft(p, feed: f) { return r.text }
        return nil
    }

    static func w0(_ d: EventDraft?) -> EventWrite? { body(d) }

    static func body(_ d: EventDraft?) -> EventWrite? {
        guard let d, case .success(let w) = EventForm.wire(
            d, today: now, calendar: cal, guests: GuestTerms(open: true, owner: "me@example.com"))
        else { return nil }
        return w
    }

    // MARK: the gate

    /// MAIN'S INVENTORY, spelled out: the seventeen tools from before the
    /// calendar tool existed, in the order the model is shown them. With the
    /// Calendar page off the assistant is sent exactly these.
    static let mains = [
        "search_mail", "get_thread", "get_updates", "explain_triage", "get_records", "search_contacts",
        "show_emails", "set_status", "create_sender_rule", "save_draft", "archive_message", "label_message",
        "send_email", "unsubscribe_sender", "product_help", "list_shortcuts", "find_setting",
    ]

    static func theToolIsOfferedOnlyWithTheCalendar() {
        let off = EventProposals.inventory(mains, calendar: false, after: "unsubscribe_sender")
        expect(off == mains, "off: main's tools, in main's order, and nothing else")
        expect(!off.contains(EventProposals.toolName), "off: no calendar tool")
        let on = EventProposals.inventory(mains, calendar: true, after: "unsubscribe_sender")
        expect(on.count == mains.count + 1, "on: exactly one more tool")
        expect(on.filter { $0 != EventProposals.toolName } == mains, "on: the others unmoved")
        expect(on.firstIndex(of: EventProposals.toolName) == mains.firstIndex(of: "unsubscribe_sender").map { $0 + 1 },
               "on: after the last acting tool")
        // Even handed a list that already names it, off means off.
        expect(EventProposals.inventory(on, calendar: false, after: "unsubscribe_sender") == mains,
               "off strips the tool from any list")
        expect(EventProposals.inventory(on, calendar: true, after: "unsubscribe_sender") == on, "on is stable")
        expect(EventProposals.inventory(mains, calendar: true, after: "no_such_tool").last == EventProposals.toolName,
               "an unknown neighbour puts it last, never drops it")
        expect(!EventProposals.mayRun(EventProposals.toolName, calendar: false),
               "off: a call to it is a call to a tool that does not exist")
        expect(EventProposals.mayRun(EventProposals.toolName, calendar: true), "on: it runs")
        expect(EventProposals.mayRun("search_mail", calendar: false), "every other tool is as it was")
        expect(EventProposals.toolName == "create_calendar_event", "the wire name")
    }

    // MARK: the call

    static func readingTheCall() {
        let full = EventProposals.read(input([
            "title": .string("  Dinner with Steven "), "start": .string("2026-10-09T16:00:00"),
            "end": .string("2026-10-09T18:00:00"), "all_day": .bool(false), "calendar": .string("Family"),
            "location": .string("Zuni"),
            "guests": .array([.string("steven@example.com"), .string(" ana@example.com ")]),
        ]))
        guard case .success(let p) = full else {
            expect(false, "a whole call reads")
            return
        }
        expect(p.title == "Dinner with Steven", "the title, trimmed")
        expect(p.start == "2026-10-09T16:00:00" && p.end == "2026-10-09T18:00:00" && !p.allDay, "the times")
        expect(p.calendar == "Family" && p.location == "Zuni", "the rest")
        expect(p.guests == ["steven@example.com", "ana@example.com"], "the guests, trimmed")
        func fails(_ i: [String: JSONValue]) -> String? {
            if case .failure(let r) = EventProposals.read(i) { return r.text }
            return nil
        }
        expect(fails(["start": .string("2026-10-09")]) == "missing title", "no title")
        expect(fails(["title": .string("   "), "start": .string("2026-10-09")]) == "missing title", "a blank title")
        expect(fails(["title": .string("x")]) == "missing start", "no start")
        expect(fails(["title": .string("x"), "start": .number(5)]) == "missing start", "a start that is not text")
        expect(fails(["title": .string("x"), "start": .string("2026-10-09"), "guests": .array([.number(1)])]) != nil,
               "a guest list of something else is refused, not guessed at")
        expect(fails(["title": .string("x"), "start": .string("2026-10-09"), "guests": .bool(true)]) != nil,
               "and guests that are not a list")
        func reads(_ i: [String: JSONValue]) -> EventProposal? {
            if case .success(let p) = EventProposals.read(i) { return p }
            return nil
        }
        let minimal = reads(["title": .string("x"), "start": .string("2026-10-09T16:00:00")])
        expect(minimal?.end == nil && minimal?.allDay == false && minimal?.guests.isEmpty == true
            && minimal?.calendar == nil, "title and start are enough")
        expect(reads(["title": .string("x"), "start": .string("2026-10-09"), "all_day": .string("true")])?.allDay == true,
               "a quoted boolean is read")
        expect(reads(["title": .string("x"), "start": .string("2026-10-09"), "guests": .string("a@b.co, c@d.co")])?.guests
            == ["a@b.co", "c@d.co"], "a lone string of guests is split")
        expect(reads(["title": .string("x"), "start": .string("2026-10-09"), "guests": .null])?.guests.isEmpty == true,
               "a null guest list is none")
        expect(reads(["title": .string("x"), "start": .string("2026-10-09"), "unknown": .string("y")]) == nil,
               "a field it has never heard of is refused, not ignored (verify7 E14)")
        expect(reads(["title": .string("x"), "start": .string("2026-10-09"), "unknown": .null]) != nil,
               "unless it holds nothing")
    }

    // MARK: the draft

    static func aTimedProposalBecomesTheDraft() {
        let d = made(proposal(end: "2026-10-09T18:30:00", location: "Zuni"))
        expect(d?.title == "Dinner with Steven" && d?.calendarId == "me@example.com", "title, on the primary calendar")
        expect(d?.date == "Fri Oct 9" && d?.start == "4pm" && d?.end == "6:30pm" && d?.allDay == false,
               "the form's own fields: \(d?.date ?? "") \(d?.start ?? "") \(d?.end ?? "")")
        expect(d?.endDate == "", "an end the same day leaves the end day following")
        expect(d?.location == "Zuni" && d?.guests == "", "the rest, and no guests")
        expect(d?.notes == "" && w0(d)?.description == nil, "the notes line opens empty, and the body carries none")
        expect(d?.titleRead.on == false, "a proposed title is never read for a day or a time")
        let w = body(d)
        expect(w?.start == "2026-10-09T16:00:00-07:00" && w?.end == "2026-10-09T18:30:00-07:00",
               "what the user's press would send: \(w?.start ?? "") to \(w?.end ?? "")")
        expect(w?.title == "Dinner with Steven" && w?.guests == nil && w?.notify == nil, "and it tells nobody")
        // One hour when no end is said.
        expect(body(made(proposal()))?.end == "2026-10-09T17:00:00-07:00", "a timed event defaults to an hour")
        // An offset is honoured, and said back in the user's own zone.
        let utc = made(proposal(start: "2026-10-09T23:00:00Z", end: "2026-10-10T00:30:00Z"))
        expect(utc?.date == "Fri Oct 9" && utc?.start == "4pm" && utc?.end == "5:30pm",
               "23:00Z is 4pm in San Francisco: \(utc?.start ?? "")")
        let east = made(proposal(start: "2026-10-09T19:00:00-04:00", end: "2026-10-09T20:00:00-04:00"))
        expect(east?.start == "4pm" && east?.end == "5pm", "7pm Eastern is 4pm here")
        // Overnight and multi-day.
        let late = made(proposal(start: "2026-10-09T22:00:00", end: "2026-10-10T02:00:00"))
        expect(late?.start == "10pm" && late?.end == "2am" && late?.endDate == "", "overnight is said as it is typed")
        expect(body(late)?.end == "2026-10-10T02:00:00-07:00", "and ends the next morning")
        let long = made(proposal(start: "2026-10-09T18:00:00", end: "2026-10-11T10:00:00"))
        expect(long?.endDate == "Sun Oct 11", "a run past the next morning names its end day: \(long?.endDate ?? "")")
        expect(body(long)?.end == "2026-10-11T10:00:00-07:00", "and saves it")
        // Next year carries its year.
        let far = made(proposal(start: "2027-03-02T09:00:00"))
        expect(far?.date == "Tue Mar 2 2027" && body(far)?.start == "2027-03-02T09:00:00-08:00",
               "another year, and that day's own offset")
        expect(made(proposal(start: "2026-10-09T16:15")) != nil, "minutes without seconds read")
    }

    static func anAllDayProposalBecomesTheDraft() {
        let one = made(proposal("Rent", start: "2026-10-09", allDay: true))
        expect(one?.allDay == true && one?.date == "Fri Oct 9", "one day")
        let w1 = body(one)
        expect(w1?.all_day == true && w1?.start == "2026-10-09" && w1?.end == "2026-10-09", "ends on itself")
        let trip = made(proposal("Lisbon", start: "2026-10-14", end: "2026-10-19", allDay: true))
        expect(trip?.date == "Wed Oct 14" && trip?.endDate == "Mon Oct 19", "a run names its LAST day")
        let w = body(trip)
        expect(w?.start == "2026-10-14" && w?.end == "2026-10-19", "inclusive, as the wire wants it")
        expect(refusal(proposal("x", start: "2026-10-14", end: "2026-10-12", allDay: true)) == "the event ends before it starts",
               "a last day before the first")
        expect(refusal(proposal("x", start: "2026-10-09T16:00:00", allDay: true)) != nil,
               "all day with a time is mixed precision")
        expect(refusal(proposal("x", start: "2026-10-09", end: "2026-10-10T10:00:00", allDay: true)) != nil,
               "and so is a timed end")
        expect(refusal(proposal("x", start: "2026-10-09")) != nil, "a date without all_day is refused, not guessed")
        expect(refusal(proposal("x", start: "2026-02-30", allDay: true)) != nil, "February 30 is not a date")
    }

    static func theCalendarItGoesOn() {
        expect(made(proposal(calendar: "Family"))?.calendarId == "family", "a calendar by name")
        expect(made(proposal(calendar: "family"))?.calendarId == "family", "folded")
        expect(made(proposal(calendar: "me@example.com"))?.calendarId == "me@example.com", "or by id")
        expect(made(proposal(calendar: "primary"))?.calendarId == "me@example.com", "\"primary\" is the primary")
        expect(refusal(proposal(calendar: "Holidays")) == EventProposals.unknownCalendarText,
               "a calendar the user cannot write to is not one")
        expect(refusal(proposal(calendar: "Work")) == EventProposals.unknownCalendarText, "nor one that is not there")
        expect(!EventProposals.unknownCalendarText.contains("Family") && !EventProposals.unknownCalendarText.contains("Jamie"),
               "the refusal names none of the user's calendars: the assistant does not read the calendar")
        // NO FEED IS NOT "NO CALENDAR" (verify7 E12): the feed is loaded by
        // the Calendar page, so before that page was opened there is none,
        // and the tool loads it once. Still none after that: the load failed.
        expect(refusal(proposal(), feed: nil) == EventProposals.calendarUnreachableText,
               "no feed is a calendar that could not be reached")
        expect(refusal(proposal(), feed: nil) != EventProposals.noCalendarText
            && !EventProposals.calendarUnreachableText.lowercased().contains("connect google"),
               "and never an instruction to connect one")
        expect(EventProposals.mustLoad(feed: nil), "with no feed the tool loads it before judging")
        expect(!EventProposals.mustLoad(feed: feed) && !EventProposals.mustLoad(feed: TimelineFeed(ghosts: [], events: [])),
               "a feed that is there is used as it is, even one that lists nothing")
        expect(refusal(proposal(), feed: TimelineFeed(ghosts: [], events: [])) == EventProposals.noCalendarText,
               "an older daemon lists none")
        var readOnly = TimelineFeed(ghosts: [], events: [])
        readOnly.calendars = [calendarRow("holidays", "Holidays", writable: false)]
        expect(refusal(proposal(), feed: readOnly) == EventProposals.noCalendarText, "only read-only calendars")
    }

    static func guestsAreRealGuestsWhereTheyMayBe() {
        let d = made(proposal(guests: ["steven@example.com", "Ana <ana@example.com>"]))
        expect(EventGuests.read(d?.guests ?? "").addresses == ["steven@example.com", "ana@example.com"],
               "real guests on the line, as addresses: \(d?.guests ?? "")")
        expect(d?.guests.hasSuffix(",") == true && EventGuests.read(d?.guests ?? "").unread.isEmpty, "all whole pills")
        let w = body(d)
        expect(w?.guests == ["steven@example.com", "ana@example.com"], "the body the user's press would send names them")
        expect(w?.notify == nil, "and says nothing about telling them: only the control pressed decides that")
        // The control the pane shows for this draft says it invites.
        let controls = EventGuests.controls(editing: false, before: 0, after: 2)
        expect(controls.primary.label.contains("invite"), "the pane's control says it will invite: \(controls.primary.label)")
        expect(!EventGuests.enterMaySave(controls), "and plain Enter saves nothing")
        expect(refusal(proposal(calendar: "Family", guests: ["steven@example.com"])) == EventProposals.cannotInviteText,
               "a calendar that may not invite refuses guests")
        expect(made(proposal(calendar: "Family")) != nil, "and takes the event without them")
        expect(refusal(proposal(guests: ["steven"])) == "every guest must be a whole email address", "a name is not a guest")
        expect(refusal(proposal(guests: ["me@example.com"])) == EventDraftProblem.guestIsYou.text,
               "the user's own address, in the pane's own words")
        let crowd = (1...51).map { "g\($0)@example.com" }
        expect(refusal(proposal(guests: crowd)) == EventDraftProblem.tooManyGuests.text, "51 guests")
        expect(made(proposal(guests: Array(crowd.prefix(50)))) != nil, "50 is the most")
        // A daemon older than guests: no guests line, so no proposed guests.
        var old = TimelineFeed(ghosts: [], events: [])
        old.calendars = [calendarRow("me@example.com", "Jamie", primary: true)]
        expect(refusal(proposal(guests: ["steven@example.com"]), feed: old) == EventProposals.cannotInviteText,
               "a daemon that cannot invite refuses guests")
    }

    /// Every one of these answers with words and makes NO draft.
    static func aBadProposalIsAnErrorNotAHalfFilledPane() {
        let bad: [(EventProposal, String)] = [
            (proposal(String(repeating: "a", count: 201)), EventProposals.longTitleText),
            (proposal(start: "friday at 4"), "start must be a date and time like 2026-10-09T16:00:00, or set all_day to true for a date"),
            (proposal(start: "2026-10-09T16:00:00", end: "later"), "end must be a date and time like 2026-10-09T17:00:00"),
            (proposal(start: "2026-10-09T16:00:00", end: "2026-10-09T15:00:00"), "the event must end after it starts"),
            (proposal(start: "2026-10-09T16:00:00", end: "2026-10-09T16:00:00"), "the event must end after it starts"),
            (proposal(start: "2026-10-09T16:00:00", end: "2026-10-10"), "end must be a date and time like 2026-10-09T17:00:00"),
            (proposal(start: "2026-13-40T16:00:00"), "start must be a date and time like 2026-10-09T16:00:00, or set all_day to true for a date"),
        ]
        for (p, want) in bad {
            expect(refusal(p) == want, "\"\(p.start)\" / \(p.title.prefix(12)): \(refusal(p) ?? "drafted!")")
            expect(made(p) == nil, "and no draft")
        }
        expect(made(proposal(String(repeating: "a", count: 200))) != nil, "200 characters of title is allowed")
        for text in [EventProposals.draftedText, EventProposals.paneBusyText, EventProposals.noCalendarText,
                     EventProposals.unknownCalendarText, EventProposals.cannotInviteText,
                     EventProposals.questionOpenText, EventProposals.promptLine, EventProposals.noNotesText,
                     EventProposals.calendarUnreachableText, EventProposals.askClosedText,
                     EventProposals.writingMailText, EventProposals.draftedParkedText,
                     EventProposals.keysLockedText, EventProposals.longTitleText,
                     EventProposals.staleRunText, EventProposals.guestsNeedAClickText,
                     EventProposals.guestsNeedAClickHint,
                     EventProposals.longLocationText, EventProposals.unknownFieldText("x")] {
            expect(!text.contains("\u{2014}") && !text.contains("\u{2013}"), "no dash the house style forbids: \(text.prefix(24))")
        }
    }

    // MARK: the pane

    static func aPaneInUseRefuses() {
        func refusal(exists: Bool, touched: Bool = false, editable: Bool = true, deleteAsked: Bool = false) -> String? {
            EventProposals.paneRefusal(paneExists: exists, touched: touched, editable: editable, deleteAsked: deleteAsked)
        }
        expect(refusal(exists: false) == nil, "no pane: the proposal opens one")
        expect(refusal(exists: true) == nil, "an untouched pane gives way")
        expect(refusal(exists: true, touched: true) == EventProposals.paneBusyText,
               "a pane with typed content is never replaced")
        expect(refusal(exists: true, touched: false, editable: false) == EventProposals.paneBusyText,
               "nor one whose write is out or in doubt")
        expect(refusal(exists: false, deleteAsked: true) == EventProposals.questionOpenText,
               "nor under the delete question")
        expect(EventProposals.paneBusyText.contains("finish or close the open event first"),
               "the words the assistant relays")
        // A PROPOSED pane counts as typed: it opens touched, because what it
        // opened with is a blank event and the proposal is what is in it.
        let blank = EventForm.newDraft(day: now, now: now, calendarId: "me@example.com", calendar: cal)
        if let d = made(proposal()) {
            var initial = blank
            initial.titleRead = d.titleRead
            expect(d != initial, "a proposal differs from the blank event its pane opened with")
            expect(EventPaneTravel.leaving(open: true, touched: d != initial) == .park,
                   "so leaving the page parks it instead of throwing it away")
            expect(EventPaneTravel.composer(open: true, parked: false, touched: d != initial).allow == false,
                   "and a composer does not replace it")
        } else {
            expect(false, "the proposal drafts")
        }
    }

    /// THE TITLE IS THE ONLY PROSE. Notes in any spelling refuse the call
    /// (so the model cannot think they were saved), and nothing reaches the
    /// draft or the body an "add and invite" would send.
    static func theAssistantWritesNoNotes() {
        func fails(_ extra: [String: JSONValue]) -> String? {
            var i: [String: JSONValue] = ["title": .string("Dinner"), "start": .string("2026-10-09T19:00:00")]
            for (k, v) in extra { i[k] = v }
            if case .failure(let r) = EventProposals.read(i) { return r.text }
            return nil
        }
        for key in ["notes", "description", "body", "details", "agenda", "note", "summary", "message",
                    "Notes", "DESCRIPTION", "Body", "Summary", "Message", "NOTE"] {
            expect(fails([key: .string("Looking forward to it")]) == EventProposals.noNotesText,
                   "\(key): refused, in words")
        }
        // ANY OTHER FIELD THE TOOL DOES NOT HAVE is refused by name (verify7
        // E14): nothing is dropped in silence, so the model cannot tell the
        // user something was saved that was not.
        for key in ["comment", "attendees", "reminder", "Title", "url", "recurrence", "notes_for_guests"] {
            let said = fails([key: .string("x")])
            expect(said == EventProposals.unknownFieldText(key) && said?.contains("unknown field") == true,
                   "\(key): an unknown field, said by name: \(said ?? "accepted!")")
        }
        expect(fails(["comment": .null]) == nil && fails(["comment": .string("")]) == nil,
               "a key holding nothing carried nothing")
        expect(fails(["attendees": .array([.string("a@b.co")])]) == EventProposals.unknownFieldText("attendees"),
               "guests under another name are not quietly lost either")
        expect(fails(["zzz": .string("x"), "notes": .string("y")]) == EventProposals.noNotesText,
               "notes are refused as notes even beside another unknown key")
        expect(EventProposals.unknownFieldText(String(repeating: "k", count: 500) + "\nsecond line").count < 220,
               "a key is said back short and on one line")
        expect(!EventProposals.unknownFieldText("a\nb").contains("\n"), "never with a line break of the model's")
        for known in EventProposals.knownKeys {
            expect(!EventProposals.notesKeys.contains(known), "\(known) is a field the tool has")
        }
        expect(fails(["notes": .number(3)]) == EventProposals.noNotesText, "notes of any type are refused")
        expect(fails(["notes": .null]) == nil && fails(["notes": .string("")]) == nil,
               "an empty or null notes field is nothing to refuse")
        expect(fails([:]) == nil, "and a call without them reads")
        expect(EventProposals.noNotesText.contains("type notes in the event pane themselves"),
               "the refusal says who may write them")
        // With guests: the body the user's press would send has no description.
        let d = made(proposal(guests: ["steven@example.com"]))
        expect(d?.notes == "", "a proposal with guests opens with empty notes")
        expect(body(d)?.description == nil && body(d)?.guests == ["steven@example.com"],
               "so nothing the model wrote but the title can reach a guest")
        expect(EventProposals.promptLine.contains("no\n") || EventProposals.promptLine.contains("no notes or description"),
               "the prompt says so")
        expect(EventProposals.promptLine.contains("The title and the place are the only text you supply"),
               "and that the title and the place are the only text it supplies")
    }

    /// verify7 D3 / E14: the title and the place are one line of at most 200
    /// characters each, and over that is a refusal, never a trim.
    static func theTitleAndThePlaceAreOneShortLine() {
        let cap = EventProposals.maxLine
        expect(cap == 200 && cap == EventForm.maxTitle, "the daemon's own cap")
        expect(made(proposal(location: String(repeating: "p", count: cap)))?.location.count == cap,
               "200 characters of place is allowed, whole")
        expect(refusal(proposal(location: String(repeating: "p", count: cap + 1))) == EventProposals.longLocationText,
               "201 is refused")
        expect(made(proposal(location: String(repeating: "p", count: cap + 1))) == nil, "and makes no draft: not trimmed")
        expect(refusal(proposal(location: String(repeating: "p", count: 7_199))) == EventProposals.longLocationText,
               "the reviewer's 7,199 characters")
        for place in ["Cafe\nSee https://evil.test", "Cafe\r\nRoom 2", "Cafe\tRoom 2", "Cafe\u{2028}Room", "Cafe\u{0}"] {
            expect(refusal(proposal(location: place)) == EventProposals.longLocationText,
                   "a place of more than one line is refused: \(place.debugDescription)")
        }
        for title in ["Multi\nline\ttitle", "Dinner\nwith a second line", "Dinner\tparty", "Dinner\u{2029}x"] {
            expect(refusal(proposal(title)) == EventProposals.longTitleText,
                   "a title of more than one line is refused: \(title.debugDescription)")
            expect(made(proposal(title)) == nil, "and makes no draft")
        }
        expect(made(proposal("Dîner chez Zoé", location: "Café de Flore, 172 Bd Saint-Germain"))?.location
            == "Café de Flore, 172 Bd Saint-Germain", "an ordinary place is kept exactly")
        // Counted as the daemon counts (scalars), like the title.
        expect(made(proposal(location: String(repeating: "é", count: cap))) != nil, "200 accented letters are 200")
        // Through the call itself.
        func call(_ extra: [String: JSONValue]) -> String? {
            var i: [String: JSONValue] = ["title": .string("Dinner"), "start": .string("2026-10-09T19:00:00")]
            for (k, v) in extra { i[k] = v }
            switch EventProposals.read(i) {
            case .failure(let r): return r.text
            case .success(let p): return refusal(p)
            }
        }
        expect(call(["location": .string(String(repeating: "p", count: 201))]) == EventProposals.longLocationText,
               "a long place in a call is refused")
        expect(call(["title": .string("a\nb")]) == EventProposals.longTitleText, "a two-line title in a call is refused")
        expect(call(["location": .string("  Cafe  \n")]) == nil, "space and a line break around the place are not the place")
    }

    /// verify7 D2: when a proposal may be drafted, where its pane goes, and
    /// what its keys do.
    static func itNeverTakesTheScreenFromSomebody() {
        func land(ask: Bool = true, writing: Bool = false, thread: Bool = false) -> Result<EventProposals.Landing, ProposalRefusal> {
            EventProposals.landing(askBarOpen: ask, writingMail: writing, threadOpen: thread)
        }
        func text(_ r: Result<EventProposals.Landing, ProposalRefusal>) -> String? {
            if case .failure(let refusal) = r { return refusal.text }
            return nil
        }
        expect(land() == .success(.calendarPage), "the bar open, nothing being written: the Calendar page")
        expect(text(land(ask: false)) == EventProposals.askClosedText,
               "the bar closed (a citation clicked, \"edit in composer\"): refused")
        expect(text(land(ask: false, thread: true)) == EventProposals.askClosedText, "whatever else is on screen")
        expect(text(land(writing: true)) == EventProposals.writingMailText, "a typed-in composer is never closed")
        expect(text(land(writing: true, thread: true)) == EventProposals.writingMailText, "nor a typed-in inline reply")
        expect(text(land(ask: false, writing: true)) == EventProposals.askClosedText, "the bar is asked about first")
        expect(land(thread: true) == .success(.parked), "an open email is not closed: the pane is parked")
        expect(EventProposals.draftedText(.calendarPage) == EventProposals.draftedText, "what the model is told, on the page")
        let parked = EventProposals.draftedText(.parked)
        expect(parked.contains("waiting on the Calendar page") && parked.contains("Nothing has been added")
            && parked.contains("nobody has been invited"), "and parked: where it waits, and what did not happen")
        expect(EventProposals.askClosedText.contains("Nothing was drafted")
            && EventProposals.writingMailText.contains("Nothing was drafted"), "a refusal says nothing was drafted")
        // THE KEYS. ↵ and ⌘↵ on an untouched proposal do nothing.
        expect(!EventProposals.keysMayAct(proposed: true, engaged: false),
               "⌘↵ on a proposal nobody has clicked or typed in does nothing")
        expect(EventProposals.keysMayAct(proposed: true, engaged: true), "once the user has, the keys are theirs")
        expect(EventProposals.keysMayAct(proposed: false, engaged: false), "a pane the user opened is theirs from the start")
        expect(EventProposals.keysMayAct(proposed: false, engaged: true), "always")
        expect(EventProposals.keysLockedText.contains("click add"), "and the toast says what does work")
        // GUESTS A MODEL NAMED (verify8 V8-3): ⌘↵ never saves a proposal
        // that would tell guests, clicked in or not. Only the button does.
        for engaged in [false, true] {
            expect(!EventProposals.commandEnterMaySave(proposed: true, engaged: engaged, tellsGuests: true),
                   "a proposal with guests: ⌘↵ never invites (engaged \(engaged))")
        }
        expect(!EventProposals.commandEnterMaySave(proposed: true, engaged: false, tellsGuests: false),
               "a proposal with no guests keeps the old rule: nothing until the user is in the pane")
        expect(EventProposals.commandEnterMaySave(proposed: true, engaged: true, tellsGuests: false),
               "and then ⌘↵ adds it")
        for engaged in [false, true] {
            for guests in [false, true] {
                expect(EventProposals.commandEnterMaySave(proposed: false, engaged: engaged, tellsGuests: guests),
                       "a pane the user opened: ⌘↵ is theirs, guests or not")
            }
        }
        expect(EventProposals.guestsNeedAClickText.contains("Only a click")
            && EventProposals.guestsNeedAClickHint.contains("Only a click")
            && EventProposals.guestsNeedAClickHint.contains("⌘↵ does not"), "the toast and the hint line say so")
        // The pane's own measure of "would tell guests" is the one plain
        // Enter already goes by: any guest on a new event.
        expect(!EventGuests.enterMaySave(EventGuests.controls(editing: false, before: 0, after: 1)),
               "one guest on a new event is a save that tells")
        expect(EventGuests.enterMaySave(EventGuests.controls(editing: false, before: 0, after: 0)),
               "none is not")
    }

    /// verify8 V8-1: the run is asked about AFTER the calendar is read. A
    /// New chat or an account switch during that wait gets fixed words and
    /// no pane.
    static func aStaleRunDraftsNothing() {
        expect(EventProposals.runStillCurrent(live: true, epochAsked: 4, epochNow: 4), "the same run, the same mailbox: go on")
        expect(!EventProposals.runStillCurrent(live: false, epochAsked: 4, epochNow: 4), "New chat during the load: stop")
        expect(!EventProposals.runStillCurrent(live: true, epochAsked: 4, epochNow: 5), "the mailbox switched: stop")
        expect(!EventProposals.runStillCurrent(live: false, epochAsked: 4, epochNow: 5), "both: stop")
        let said = EventProposals.staleRunText
        expect(said.contains("Nothing was drafted") && said.contains("Do not try again"),
               "fixed words: nothing happened, and no retry into the new conversation")
        expect(!said.contains("\u{2014}") && !said.contains("\u{2013}"), "house style")
    }

    /// verify8 V8-2: a parked proposal never closes mail being written.
    static func aParkedProposalWaitsForMailBeingWritten() {
        expect(!EventPaneTravel.parkedMayReturn(proposed: true, writingMail: true),
               "a proposal stays parked while a composer or inline reply holds anything typed")
        expect(EventPaneTravel.parkedMayReturn(proposed: true, writingMail: false), "and comes back once it does not")
        expect(EventPaneTravel.parkedMayReturn(proposed: false, writingMail: true),
               "a pane the USER typed comes back when they come to the page, as before")
        expect(EventPaneTravel.parkedMayReturn(proposed: false, writingMail: false), "always")
    }

    static func whatTheModelIsTold() {
        let said = EventProposals.draftedText
        expect(said.contains("drafted") && said.contains("waiting for the user to confirm"), "drafted, and waiting")
        expect(said.contains("Nothing has been added") && said.contains("nobody has been invited"),
               "and what did not happen, in as many words")
        expect(EventProposals.promptLine.contains(EventProposals.toolName), "the prompt names the tool")
        expect(EventProposals.promptLine.contains("invites nobody"), "and says it invites nobody")
    }
}
