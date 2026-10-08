// The event editor's arithmetic, against fixtures (Lib/EventDraft.swift).
//
// This is the first code in Passband that writes to somebody's real calendar,
// so each of these is a wrong event on a real calendar: a dinner saved at
// 7:45am, a two-day trip saved as one day because the inclusive end was sent
// as Google's exclusive one, an edit to a title that quietly moved the times,
// a cleared location that was never cleared. Today is the mock's: Monday
// October 5 2026, 3:40pm, in San Francisco.

import Foundation

@main
struct EventDraftTests {
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
        timesRead()
        endsReadAgainstTheStart()
        datesRead()
        aTimedDraftToTheWire()
        anAllDayDraftToTheWireWithTheInclusiveEnd()
        validation()
        summaries()
        newDrafts()
        sweptDrafts()
        aClickedDayMovesTheDraft()
        theDraftOnTheGrid()
        editingRoundTrips()
        patchesSendOnlyWhatMoved()
        writePaths()
        writableCalendars()
        answersLandInTheFeed()
        refusalsAreFixedSentences()
        anUncertainWriteIsSettledByTheFeed()
        anAnswerAfterTheGateClosedIsNobodys()
        aTypedPaneSurvivesAPageChange()
        theWave3WireDecodes()

        if failures > 0 {
            print("FAILED: \(failures) of \(checks) checks")
            exit(1)
        }
        print("ok: \(checks) checks passed")
    }

    // MARK: fixtures

    static func at(_ s: String) -> Date { AgendaTime.parse(s, zone: nil, calendar: cal)!.date }
    static func day(_ s: String) -> Date { cal.startOfDay(for: at(s + "T12:00:00-07:00")) }

    static func draft(
        _ title: String = "Dreamers Dinner", date: String = "Thu Oct 8", start: String = "7:45pm",
        end: String = "9pm", allDay: Bool = false, endDate: String = "", location: String = "",
        notes: String = "", calendarId: String? = "primary"
    ) -> EventDraft {
        var d = EventDraft()
        (d.title, d.date, d.start, d.end, d.allDay, d.endDate) = (title, date, start, end, allDay, endDate)
        (d.location, d.notes, d.calendarId) = (location, notes, calendarId)
        return d
    }

    static func event(
        _ id: String = "e1", title: String? = "Dreamers Dinner", start: String, end: String?,
        allDay: Bool = false, calendar: String = "primary", location: String? = nil,
        description: String? = nil, editable: Bool? = true
    ) -> TimelineEvent {
        TimelineEvent(
            id: id, calendar_id: calendar, calendar_name: "Personal", title: title, start: start,
            end: end, all_day: allDay, start_timezone: "America/Los_Angeles", location: location,
            status: .confirmed, self_response: nil, busy: true, organizer: nil, html_link: nil,
            color: "#039be5", editable: editable, description: description)
    }

    static func wire(_ d: EventDraft) -> EventWrite? {
        if case .success(let w) = EventForm.wire(d, today: now, calendar: cal) { return w }
        return nil
    }

    static func problem(_ d: EventDraft) -> EventDraftProblem? { EventForm.problem(d, today: now, calendar: cal) }

    static func patch(_ d: EventDraft, from initial: EventDraft) -> EventPatch? {
        if case .success(let p) = EventForm.patch(d, from: initial, today: now, calendar: cal) { return p }
        return nil
    }

    // MARK: typed times

    static func timesRead() {
        func t(_ s: String) -> TimeOfDay? { EventForm.parseTime(s) }
        expect(t("7pm") == TimeOfDay(minutes: 19 * 60, explicit: true), "7pm")
        expect(t("7:45 pm")?.minutes == 19 * 60 + 45, "7:45 pm")
        expect(t("7.45pm")?.minutes == 19 * 60 + 45, "7.45pm")
        expect(t("745p")?.minutes == 19 * 60 + 45, "745p")
        expect(t("7a")?.minutes == 7 * 60, "7a")
        expect(t("7 a.m.")?.minutes == 7 * 60, "7 a.m.")
        expect(t("19:30") == TimeOfDay(minutes: 19 * 60 + 30, explicit: true), "19:30 is a 24-hour clock")
        expect(t("1930")?.minutes == 19 * 60 + 30, "1930")
        expect(t("07:00") == TimeOfDay(minutes: 7 * 60, explicit: true), "a leading zero is a 24-hour clock")
        expect(t("0:30") == TimeOfDay(minutes: 30, explicit: true), "0:30 is half past midnight")
        expect(t("noon")?.minutes == 12 * 60 && t("midnight")?.minutes == 0, "noon and midnight")
        expect(t("12am")?.minutes == 0 && t("12pm")?.minutes == 12 * 60, "12am and 12pm")
        expect(t("7") == TimeOfDay(minutes: 7 * 60, explicit: false), "a bare 7 is the morning")
        expect(t("11:30") == TimeOfDay(minutes: 11 * 60 + 30, explicit: false), "a bare 11:30 too")
        expect(t("3") == TimeOfDay(minutes: 15 * 60, explicit: false), "a bare 3 is the afternoon")
        expect(t("6:15")?.minutes == 18 * 60 + 15, "a bare 6:15 is the evening")
        expect(t("12") == TimeOfDay(minutes: 12 * 60, explicit: false), "a bare 12 is noon")
        for bad in ["", "13pm", "0am", "25:00", "7:5", "7:60", "lunchtime", "7pmish", "12345", ":30"] {
            expect(t(bad) == nil, "\"\(bad)\" is not a time")
        }
    }

    /// A bare hour after a start is the soonest one after it: an evening end
    /// after an evening start, the small hours after a late one.
    static func endsReadAgainstTheStart() {
        func e(_ s: String, after h: Int) -> Int? { EventForm.parseEnd(s, after: h * 60) }
        expect(e("9", after: 19) == 21 * 60, "9 after 7pm is 9pm")
        expect(e("2", after: 22) == 2 * 60, "2 after 10pm is 2am")
        expect(e("5", after: 9) == 17 * 60, "5 after 9am is 5pm")
        expect(e("11", after: 9) == 11 * 60, "11 after 9am is 11am")
        expect(e("1", after: 11) == 13 * 60, "1 after 11am is 1pm")
        expect(e("8pm", after: 21) == 20 * 60, "a stated 8pm stays 8pm, whatever the start")
        expect(e("07:00", after: 22) == 7 * 60, "a 24-hour end is taken as written")
    }

    // MARK: typed dates

    static func datesRead() {
        func d(_ s: String) -> Date? { EventForm.parseDate(s, today: now, calendar: cal) }
        expect(d("Oct 8") == day("2026-10-08"), "Oct 8")
        expect(d("october 8") == day("2026-10-08"), "october 8")
        expect(d("8 Oct") == day("2026-10-08"), "8 Oct")
        expect(d("oct 8th") == day("2026-10-08") && d("8th oct") == day("2026-10-08"), "ordinals")
        expect(d("Thu Oct 8") == day("2026-10-08"), "what the card prints reads back")
        expect(d("Fri Oct 8") == day("2026-10-08"), "a wrong weekday beside a date is ignored")
        expect(d("Thu, Oct 8, 2026") == day("2026-10-08"), "commas")
        expect(d("October 8 2027") == day("2027-10-08"), "a year")
        expect(d("Thu Oct 8 2027") == day("2027-10-08"), "what the card prints for another year")
        expect(d("10/8") == day("2026-10-08"), "10/8, month first")
        expect(d("10/8/27") == day("2027-10-08"), "a two-digit year")
        expect(d("2026-10-08") == day("2026-10-08"), "ISO")
        expect(d("today") == day("2026-10-05") && d("tomorrow") == day("2026-10-06"), "today and tomorrow")
        expect(d("fri") == day("2026-10-09"), "a weekday is the next one")
        expect(d("monday") == day("2026-10-05"), "today's weekday is today")
        expect(d("sun") == day("2026-10-11"), "Sunday is this coming one")
        expect(d("Jan 5") == day("2027-01-05"), "a month long gone is next year's")
        expect(d("Sep 1") == day("2026-09-01"), "a few weeks back is still this year's")
        for bad in ["", "Feb 30", "13/1", "Oct", "someday", "10/8/27/1", "Oct 32", "2026-13-01"] {
            expect(d(bad) == nil, "\"\(bad)\" is not a date")
        }
    }

    // MARK: to the wire

    static func aTimedDraftToTheWire() {
        let w = wire(draft(location: "  Zuni Cafe ", notes: " table for 4 "))
        expect(w?.calendar_id == "primary" && w?.title == "Dreamers Dinner" && w?.all_day == false, "the fields")
        expect(w?.start == "2026-10-08T19:45:00-07:00", "the start with its offset")
        expect(w?.end == "2026-10-08T21:00:00-07:00", "the end with its offset")
        expect(w?.location == "Zuni Cafe" && w?.description == "table for 4", "trimmed")
        expect(wire(draft())?.location == nil && wire(draft())?.description == nil,
               "an empty location or note is absent, not an empty string")
        let late = wire(draft(start: "10pm", end: "2am"))
        expect(late?.end == "2026-10-09T02:00:00-07:00", "10pm to 2am ends the next morning")
        let bare = wire(draft(start: "7", end: "9"))
        expect(bare?.start == "2026-10-08T07:00:00-07:00" && bare?.end == "2026-10-08T09:00:00-07:00",
               "a bare 7 to 9 is the morning")
        let evening = wire(draft(start: "6", end: "9"))
        expect(evening?.end == "2026-10-08T21:00:00-07:00", "a bare 6 to 9 is the evening")
        // Across the clocks going back: each end carries its own offset.
        let fallBack = wire(draft(date: "Oct 31", start: "10pm", end: "6am"))
        expect(fallBack?.start == "2026-10-31T22:00:00-07:00" && fallBack?.end == "2026-11-01T06:00:00-08:00",
               "a night across the clocks going back carries both offsets")
        let longer = wire(draft(date: "Oct 14", start: "6:05pm", end: "8am", endDate: "Oct 16"))
        expect(longer?.end == "2026-10-16T08:00:00-07:00", "an end day, when there is one")
    }

    static func anAllDayDraftToTheWireWithTheInclusiveEnd() {
        let trip = wire(draft("Lisbon", date: "Oct 14", allDay: true, endDate: "Oct 19"))
        expect(trip?.all_day == true && trip?.start == "2026-10-14", "all day starts on its day")
        expect(trip?.end == "2026-10-19", "and ends on its LAST day, inclusive, as the timeline serves it")
        let one = wire(draft("Rent", date: "Oct 1", allDay: true))
        expect(one?.start == "2026-10-01" && one?.end == "2026-10-01", "a single day ends on itself")
        let hidden = wire(draft("Rent", date: "Oct 1", start: "garbage", end: "", allDay: true))
        expect(hidden != nil, "all day ignores the time fields it hides")
    }

    static func validation() {
        expect(problem(draft()) == nil, "a good draft has no problem")
        expect(problem(draft("   ")) == .noTitle, "a blank title")
        expect(problem(draft(String(repeating: "a", count: 200))) == nil, "200 characters is allowed")
        expect(problem(draft(String(repeating: "a", count: 201))) == .titleTooLong, "201 is not")
        expect(problem(draft(String(repeating: "é", count: 200))) == nil, "characters, not bytes")
        expect(problem(draft(calendarId: nil)) == .noCalendar, "no calendar")
        expect(problem(draft(date: "someday")) == .badDate, "an unreadable date")
        expect(problem(draft(start: "lunchtime")) == .badStart, "an unreadable start")
        expect(problem(draft(end: "later")) == .badEnd, "an unreadable end")
        expect(problem(draft(start: "7pm", end: "7pm")) == .noLength, "ending when it starts")
        expect(problem(draft(start: "7pm", end: "8pm", endDate: "Oct 7")) == .endsBeforeStart,
               "an end day before the start")
        expect(problem(draft(allDay: true, endDate: "Oct 7")) == .endsBeforeStart, "a last day before the first")
        expect(problem(draft(allDay: true, endDate: "Octember")) == .badEndDate, "an unreadable last day")
        expect(problem(draft(notes: String(repeating: "n", count: 2000))) == nil, "2,000 characters of notes")
        expect(problem(draft(notes: String(repeating: "n", count: 2001))) == .notesTooLong, "2,001 is too many")
        expect(problem(draft("", date: "someday")) == .noTitle, "the title is said first, in field order")
        let texts = [EventDraftProblem.noTitle, .titleTooLong, .noCalendar, .badDate, .badEndDate, .badStart,
                     .badEnd, .noLength, .endsBeforeStart, .notesTooLong].map(\.text)
        expect(Set(texts).count == texts.count, "every problem says something different")
        expect(texts.allSatisfy { !$0.contains("\u{2014}") }, "and no em dash anywhere")
    }

    static func summaries() {
        func s(_ d: EventDraft) -> String? {
            if case .success(let w) = EventForm.when(d, today: now, calendar: cal) {
                return EventForm.summary(w, calendar: cal)
            }
            return nil
        }
        expect(s(draft()) == "Thu Oct 8, 7:45pm to 9pm", "an evening")
        expect(s(draft(start: "10pm", end: "2am")) == "Thu Oct 8, 10pm to 2am Fri", "a night says the next day")
        expect(s(draft(date: "Oct 14", start: "6:05pm", end: "8am", endDate: "Oct 16"))
               == "Wed Oct 14, 6:05pm to 8am Fri Oct 16", "a longer run says the date")
        expect(s(draft(allDay: true)) == "Thu Oct 8, all day", "one day")
        expect(s(draft(date: "Oct 14", allDay: true, endDate: "Oct 19")) == "Wed Oct 14 to Mon Oct 19, all day",
               "a run of days")
    }

    static func newDrafts() {
        let today = EventForm.newDraft(day: day("2026-10-05"), now: now, calendarId: "primary", calendar: cal)
        expect(today.date == "Mon Oct 5" && today.start == "4pm" && today.end == "5pm",
               "today: the next whole hour, an hour long")
        expect(today.calendarId == "primary" && !today.allDay && today.title.isEmpty, "on the default calendar")
        let later = EventForm.newDraft(day: day("2026-10-08"), now: now, calendarId: "c", calendar: cal)
        expect(later.start == "9am" && later.end == "10am", "another day: 9 to 10")
        let next = EventForm.newDraft(day: day("2027-01-05"), now: now, calendarId: "c", calendar: cal)
        expect(next.date == "Tue Jan 5 2027", "another year says its year")
        let lateNight = EventForm.newDraft(
            day: day("2026-10-05"), now: at("2026-10-05T23:20:00-07:00"), calendarId: "c", calendar: cal)
        expect(lateNight.start == "11pm" && lateNight.end == "12am", "late at night: 11pm to midnight")
        expect(problem(lateNight.withTitle("x")) == nil, "and that saves, ending at midnight")
        for d in [today, later, next] {
            expect(problem(d.withTitle("x")) == nil, "a new draft saves as soon as it has a title")
        }
    }

    // MARK: wave 4: the pane's draft

    /// A span swept on the day column opens the pane with those times.
    static func sweptDrafts() {
        func swept(_ a: Int, _ b: Int) -> EventDraft {
            EventForm.newDraft(
                day: day("2026-10-08"), startMinutes: a, endMinutes: b, now: now, calendarId: "primary",
                calendar: cal)
        }
        let afternoon = swept(14 * 60 + 15, 15 * 60 + 30)
        expect(afternoon.date == "Thu Oct 8" && afternoon.start == "2:15pm" && afternoon.end == "3:30pm",
               "the day and the two times, as the form prints them")
        expect(afternoon.calendarId == "primary" && !afternoon.allDay && afternoon.endDate.isEmpty,
               "timed, on the default calendar")
        let w = wire(afternoon.withTitle("x"))
        expect(w?.start == "2026-10-08T14:15:00-07:00" && w?.end == "2026-10-08T15:30:00-07:00",
               "and it saves exactly the span swept")
        let late = swept(23 * 60, 1440)
        expect(late.start == "11pm" && late.end == "12am", "a sweep to the day's foot ends at midnight")
        expect(wire(late.withTitle("x"))?.end == "2026-10-09T00:00:00-07:00", "which is the next day's first instant")
        let early = swept(0, 15)
        expect(early.start == "12am" && early.end == "12:15am" && problem(early.withTitle("x")) == nil,
               "the day's first quarter hour saves")
    }

    /// A day clicked on the grid beside the pane sets the draft's date.
    static func aClickedDayMovesTheDraft() {
        func moved(_ d: EventDraft, _ to: String) -> EventDraft {
            EventForm.moved(d, to: day(to), today: now, calendar: cal)
        }
        let dinner = moved(draft(), "2026-10-14")
        expect(dinner.date == "Wed Oct 14" && dinner.start == "7:45pm" && dinner.end == "9pm",
               "the day moves and the times stay")
        expect(dinner.title == "Dreamers Dinner" && dinner.endDate.isEmpty, "nothing else changes")
        let trip = moved(draft(date: "Oct 14", allDay: true, endDate: "Oct 19"), "2026-10-21")
        expect(trip.date == "Wed Oct 21" && trip.endDate == "Mon Oct 26", "a run of days keeps its length")
        let oneDay = moved(draft(allDay: true), "2026-10-21")
        expect(oneDay.endDate.isEmpty, "a single all-day stays single")
        let broken = moved(draft(date: "Oct 14", allDay: true, endDate: "Octember"), "2026-10-21")
        expect(broken.date == "Wed Oct 21" && broken.endDate == "Octember",
               "an end day that does not read is left for the form to say so")
        let nextYear = moved(draft(), "2027-01-05")
        expect(nextYear.date == "Tue Jan 5 2027", "another year says its year")
        let overnight = moved(draft(date: "Oct 14", start: "6:05pm", end: "8am", endDate: "Oct 16"), "2026-10-20")
        expect(overnight.date == "Tue Oct 20" && overnight.endDate == "Thu Oct 22", "a timed run of days moves whole")
    }

    /// The draft is drawn on the month as a row of its own, where it would
    /// land, and stands in for the event being edited.
    static func theDraftOnTheGrid() {
        func row(_ d: EventDraft, color: String? = "#039be5") -> AgendaItem? {
            EventForm.provisional(d, color: color, today: now, calendar: cal)
        }
        let dinner = row(draft())
        expect(dinner?.id == EventForm.provisionalId, "one id, whatever is typed")
        expect(dinner?.title == "Dreamers Dinner" && dinner?.start == at("2026-10-08T19:45:00-07:00")
               && dinner?.end == at("2026-10-08T21:00:00-07:00") && dinner?.allDay == false,
               "on the day and at the times the fields say")
        expect(dinner?.event?.color == "#039be5", "in the chosen calendar's colour")
        expect(dinner?.fill == .outline && dinner?.isGhost == false,
               "an empty container: it is not on the calendar yet")
        expect(row(draft(""))?.title == "New event", "an untitled draft still has a name on its chip")
        expect(row(draft(date: "Oct 14"))?.start == at("2026-10-14T19:45:00-07:00"), "it moves with the date")
        expect(row(draft(start: "8pm", end: "10pm"))?.start == at("2026-10-08T20:00:00-07:00"), "and the time")
        expect(row(draft(), color: "#f4511e")?.event?.color == "#f4511e", "and recolours with the calendar")
        expect(row(draft(date: "someday")) == nil && row(draft(start: "7pm", end: "7pm")) == nil,
               "while the times do not read there is no chip, not a wrong one")
        let trip = row(draft(date: "Oct 14", allDay: true, endDate: "Oct 19"))
        expect(trip?.allDay == true && trip?.start == day("2026-10-14") && trip?.end == day("2026-10-20"),
               "an all-day run covers through its last day")

        let lunch = event("l", title: "Lunch", start: "2026-10-08T12:00:00-07:00", end: "2026-10-08T13:00:00-07:00")
        let original = event(start: "2026-10-08T19:45:00-07:00", end: "2026-10-08T21:00:00-07:00")
        let feed = [lunch, original].compactMap { Agenda.item($0, calendar: cal) }
        let creating = EventForm.withProvisional(feed, dinner, editing: nil)
        expect(creating.map(\.id) == feed.map(\.id) + [EventForm.provisionalId], "a new event's chip is added")
        let editing = EventForm.withProvisional(feed, row(draft(date: "Oct 9")), editing: original)
        expect(editing.map(\.id) == [Agenda.itemId(lunch), EventForm.provisionalId],
               "an edit's chip stands in for the event, which is not drawn twice")
        expect(EventForm.withProvisional(feed, nil, editing: original).map(\.id) == feed.map(\.id),
               "and with no chip (the times do not read) the event stays where it was")
        expect(EventForm.withProvisional(creating, dinner, editing: nil).filter { $0.id == EventForm.provisionalId }.count == 1,
               "never two drafts")
    }

    // MARK: editing

    static func editingRoundTrips() {
        let dinner = event(start: "2026-10-08T19:45:00-07:00", end: "2026-10-08T21:00:00-07:00",
                           location: "Zuni", description: "table for 4")
        let d = EventForm.draft(editing: dinner, today: now, calendar: cal)
        expect(d.date == "Thu Oct 8" && d.start == "7:45pm" && d.end == "9pm" && d.endDate.isEmpty,
               "the card fills in the event as it prints it")
        expect(d.location == "Zuni" && d.notes == "table for 4" && d.calendarId == "primary", "and the rest")
        expect(wire(d)?.start == dinner.start && wire(d)?.end == dinner.end,
               "an untouched card saves exactly the times it opened with")
        expect(patch(d, from: d)?.isEmpty == true, "and an untouched edit sends nothing")

        let party = event(start: "2026-10-10T22:00:00-07:00", end: "2026-10-11T02:00:00-07:00")
        let pd = EventForm.draft(editing: party, today: now, calendar: cal)
        expect(pd.endDate.isEmpty && wire(pd)?.end == party.end, "a night needs no end day to come back right")

        let trip = event(start: "2026-10-14", end: "2026-10-19", allDay: true)
        let td = EventForm.draft(editing: trip, today: now, calendar: cal)
        expect(td.allDay && td.date == "Wed Oct 14" && td.endDate == "Mon Oct 19",
               "an all-day run fills in its first and last days")
        expect(wire(td)?.start == "2026-10-14" && wire(td)?.end == "2026-10-19", "and saves them back inclusive")
        var longer = td
        longer.endDate = "Oct 20"
        let p = patch(longer, from: td)
        expect(p?.start == "2026-10-14" && p?.end == "2026-10-20" && p?.all_day == true,
               "a day more is sent as the new last day, inclusive")

        let flight = event(start: "2026-10-14T18:05:00-07:00", end: "2026-10-16T08:00:00+01:00")
        let fd = EventForm.draft(editing: flight, today: now, calendar: cal)
        expect(fd.endDate == "Fri Oct 16", "a run of days says its end day")
        expect(wire(fd)?.end == "2026-10-16T00:00:00-07:00", "which reads back to the same instant")
        var renamed = fd
        renamed.title = "Flight to Lisbon"
        expect(patch(renamed, from: fd) == EventPatch(title: "Flight to Lisbon"),
               "a new title alone leaves the times untouched")

        let untitled = event(title: nil, start: "2026-10-08T09:00:00-07:00", end: "2026-10-08T10:00:00-07:00")
        var placed = EventForm.draft(editing: untitled, today: now, calendar: cal)
        placed.location = "Room 4"
        expect(patch(placed, from: EventForm.draft(editing: untitled, today: now, calendar: cal))
               == EventPatch(location: .set("Room 4")),
               "an event Google holds with no title can still take a location")
    }

    static func patchesSendOnlyWhatMoved() {
        let initial = draft(location: "Zuni", notes: "table for 4")
        var d = initial
        d.title = "  Dinner at Zuni  "
        expect(patch(d, from: initial)?.json().keys.sorted() == ["title"], "a title: only the title")
        expect(patch(d, from: initial)?.title == "Dinner at Zuni", "trimmed")

        d = initial
        d.location = "  "
        let cleared = patch(d, from: initial)
        expect(cleared?.location == .clear, "an emptied location is a clear")
        expect(cleared?.json()["location"] is NSNull && cleared?.json().count == 1,
               "which goes as an explicit null, and alone")

        d = initial
        d.notes = "table for 6"
        expect(patch(d, from: initial)?.json()["description"] as? String == "table for 6", "notes are the description")

        d = initial
        d.end = "10pm"
        let moved = patch(d, from: initial)?.json() ?? [:]
        expect(moved.keys.sorted() == ["all_day", "end", "start"], "a time: start, end and all_day together")
        expect(moved["end"] as? String == "2026-10-08T22:00:00-07:00", "with the new end")

        d = initial
        d.allDay = true
        let flipped = patch(d, from: initial)
        expect(flipped?.all_day == true && flipped?.start == "2026-10-08" && flipped?.end == "2026-10-08",
               "turning all day on sends the day as dates")

        d = initial
        d.title = ""
        if case .failure(let p) = EventForm.patch(d, from: initial, today: now, calendar: cal) {
            expect(p == .noTitle, "an emptied title is refused")
        } else {
            expect(false, "an emptied title is refused")
        }
        expect(EventPatch().isEmpty && EventPatch().json().isEmpty, "nothing is nothing")
    }

    // MARK: around a write

    static func writePaths() {
        expect(CalendarWrite.path(calendarId: "primary", eventId: "abc123")
               == "/client/calendar/events/primary/abc123", "plain ids")
        expect(CalendarWrite.path(calendarId: "en.usa#holiday@group.v.calendar.google.com", eventId: "a/b c")
               == "/client/calendar/events/en.usa%23holiday@group.v.calendar.google.com/a%2Fb%20c",
               "a # or a / in an id stays inside its own segment")
        expect(CalendarWrite.segment("caf\u{E9}") == "caf%C3%A9", "non-ASCII is encoded")
    }

    static func writableCalendars() {
        let feed = TimelineFeed(calendars: [
            TimelineCalendar(id: "work", name: "Work", color: "#7986cb", primary: false, writable: true),
            TimelineCalendar(id: "me", name: "Personal", color: "#039be5", primary: true, writable: true),
            TimelineCalendar(id: "hol", name: "Holidays", color: "#0b8043", primary: false, writable: false),
            TimelineCalendar(id: "old", name: "Shared", color: nil, primary: false, writable: nil),
        ])
        expect(CalendarWrite.writable(feed).map(\.id) == ["me", "work"], "writable only, primary first")
        expect(CalendarWrite.canCreate(feed), "so a new event is offered")
        expect(CalendarWrite.writable(TimelineFeed()).isEmpty && !CalendarWrite.canCreate(TimelineFeed()),
               "an older daemon lists none, and nothing is offered")
        expect(!CalendarWrite.canCreate(nil), "nor before the first load")
        expect(!CalendarWrite.canCreate(TimelineFeed(calendars: [feed.calendars![2]])),
               "nor when no calendar is writable")
        expect(CalendarWrite.canEdit(event(start: "2026-10-08", end: nil, allDay: true)), "editable true is editable")
        expect(!CalendarWrite.canEdit(event(start: "2026-10-08", end: nil, editable: false)), "false is not")
        expect(!CalendarWrite.canEdit(event(start: "2026-10-08", end: nil, editable: nil)),
               "and an older daemon's nil is not")
        expect(CalendarWrite.name(TimelineCalendar(id: "x", name: "  ", color: nil, primary: true, writable: true))
               == "Your calendar", "a nameless primary")
    }

    static func answersLandInTheFeed() {
        let a = event("a", start: "2026-10-08T09:00:00-07:00", end: "2026-10-08T10:00:00-07:00")
        var a2 = a
        a2.title = "Renamed"
        let other = event("a", start: "2026-10-08T09:00:00-07:00", end: nil, calendar: "work")
        let base = TimelineFeed(events: [a, other])
        let added = CalendarWrite.applying(
            event("b", start: "2026-10-09T09:00:00-07:00", end: "2026-10-09T10:00:00-07:00"), to: base)
        expect(added.events?.map(\.id) == ["a", "a", "b"], "a new event is added")
        let replaced = CalendarWrite.applying(a2, to: base)
        expect(replaced.events?.count == 2 && replaced.events?.first?.title == "Renamed"
               && replaced.events?.last?.calendar_id == "work",
               "an edited one replaces itself in place, and only on its own calendar")
        expect(CalendarWrite.applying(a, to: TimelineFeed()).events?.count == 1, "a feed with no events gains them")
        expect(CalendarWrite.applying(a, to: nil).events?.count == 1, "and so does no feed")
        let removed = CalendarWrite.removing(calendarId: "primary", id: "a", from: base)
        expect(removed?.events?.map(\.calendar_id) == ["work"], "a delete takes only that calendar's event")
    }

    static func refusalsAreFixedSentences() {
        let all = [0, 400, 401, 403, 404, 500, 502, -1].flatMap { status in
            [CalendarWriteAction.create, .edit, .delete].map { CalendarWrite.failureText(status: status, $0) }
        }
        expect(all.allSatisfy { !$0.isEmpty && !$0.contains("\u{2014}") }, "every refusal says something, no em dash")
        expect(CalendarWrite.failureText(status: 404, .edit) == "That event is no longer on your calendar.",
               "a 404 on an edit: gone")
        expect(CalendarWrite.failureText(status: 404, .create) != CalendarWrite.failureText(status: 404, .edit),
               "a 404 on create is an older server, not a missing event")
        expect(CalendarWrite.failureText(status: 502, .delete).contains("not deleted"), "a refused delete says so")
        expect(CalendarWrite.failureText(status: 0, .create).contains("nothing was saved"),
               "an unreachable server: nothing was saved")
    }

    /// A 504 (fix round 1, B5): Google was asked and the answer was lost, so
    /// it is not a refusal, and the pane closes only when the reloaded page
    /// shows the change.
    static func anUncertainWriteIsSettledByTheFeed() {
        expect(CalendarWrite.uncertainStatus == 504, "the daemon's uncertain answer is a 504")
        for action in [CalendarWriteAction.create, .edit, .delete] {
            expect(
                CalendarWrite.failureText(status: 504, action) == "Google may have made this change. Refreshing…",
                "a 504 is never worded as a refusal")
        }
        expect(CalendarWrite.failureText(status: 502, .edit) != CalendarWrite.failureText(status: 504, .edit),
               "a definite refusal stays its own sentence")

        let a = event("a", start: "2026-10-08T09:00:00-07:00", end: "2026-10-08T10:00:00-07:00")
        let before = TimelineFeed(events: [a])
        let body = EventWrite(
            calendar_id: "primary", title: "Dreamers Dinner", start: "2026-10-08T19:45:00-07:00",
            end: "2026-10-08T21:00:00-07:00", all_day: false, location: nil, description: nil)
        let had = CalendarWrite.keys(before)
        expect(CalendarWrite.made(body, before: had, in: before) == nil,
               "create: an event already there with the same title is not the one made")
        let made = event("new", start: "2026-10-08T19:45:00-07:00", end: "2026-10-08T21:00:00-07:00")
        expect(CalendarWrite.made(body, before: had, in: TimelineFeed(events: [a, made]))?.id == "new",
               "create: a new event with that title on that calendar is")
        let elsewhere = event("new", start: "2026-10-08T19:45:00-07:00", end: nil, calendar: "work")
        expect(CalendarWrite.made(body, before: had, in: TimelineFeed(events: [a, elsewhere])) == nil,
               "create: not on another calendar")
        expect(CalendarWrite.made(body, before: had, in: nil) == nil, "create: no feed proves nothing")

        expect(CalendarWrite.edited(a, in: before) == nil, "edit: unchanged is not yet saved")
        var renamed = a
        renamed.title = "Renamed"
        expect(CalendarWrite.edited(a, in: TimelineFeed(events: [renamed]))?.title == "Renamed",
               "edit: the event as the page now has it")
        expect(CalendarWrite.edited(a, in: TimelineFeed(events: [])) == nil, "edit: gone is not this edit")

        expect(!CalendarWrite.gone(a, from: before), "delete: still there")
        expect(CalendarWrite.gone(a, from: TimelineFeed(events: [])), "delete: gone")
        let sameIdElsewhere = event("a", start: "2026-10-08T09:00:00-07:00", end: nil, calendar: "work")
        expect(CalendarWrite.gone(a, from: TimelineFeed(events: [sameIdElsewhere])),
               "delete: the same id on another calendar is another event")
        expect(!CalendarWrite.gone(a, from: nil) && !CalendarWrite.gone(a, from: TimelineFeed()),
               "delete: a feed that never loaded proves nothing")
    }

    /// A write's answer is the session's that sent it (fix round 1, C4): the
    /// gate closing bumps the session, an account switch the epoch, and
    /// either one leaves the answer nobody's.
    static func anAnswerAfterTheGateClosedIsNobodys() {
        let sent = CalendarWriteTicket(epoch: 3, session: 7)
        expect(sent.lands(live: CalendarWriteTicket(epoch: 3, session: 7)), "nothing dropped since: it lands")
        expect(!sent.lands(live: CalendarWriteTicket(epoch: 3, session: 8)), "the gate closed meanwhile: dropped")
        expect(!sent.lands(live: CalendarWriteTicket(epoch: 4, session: 7)), "another account: dropped")
        expect(!sent.lands(live: CalendarWriteTicket(epoch: 3, session: 9)),
               "closed and reopened: still dropped, the reopened page is a new session")
    }

    /// Fix round 1, C2: going to another page never throws a typed event
    /// away. It is parked and comes back; an untouched pane closes as before.
    static func aTypedPaneSurvivesAPageChange() {
        expect(EventPaneTravel.leaving(open: true, touched: true) == .park, "typed in: parked, not lost")
        expect(EventPaneTravel.leaving(open: true, touched: false) == .close, "untouched: closes as it always did")
        expect(EventPaneTravel.leaving(open: false, touched: true) == nil, "no pane on screen: nothing to do")
        let refused = EventPaneTravel.composer(open: true, parked: false, touched: true)
        expect(!refused.allow && !refused.closePane, "a typed pane on screen still refuses the composer")
        let gives = EventPaneTravel.composer(open: true, parked: false, touched: false)
        expect(gives.allow && gives.closePane, "an untouched one gives way to it")
        let parked = EventPaneTravel.composer(open: false, parked: true, touched: true)
        expect(parked.allow && !parked.closePane, "a parked one lets a composer open elsewhere and stays parked")
    }

    // MARK: wire

    static func theWave3WireDecodes() {
        let body = """
            {"ghosts":[],"events":[
              {"id":"a","calendar_id":"primary","calendar_name":"Personal","title":"Dinner",
               "start":"2026-10-08T19:45:00-07:00","end":"2026-10-08T21:00:00-07:00","all_day":false,
               "start_timezone":"America/Los_Angeles","location":null,"status":"confirmed",
               "self_response":null,"busy":true,"organizer":null,"html_link":null,
               "color":"#039be5","editable":true,"description":"table for 4"},
              {"id":"b","calendar_id":"primary","title":"Odd","start":"2026-10-09","end":"2026-10-09",
               "all_day":true,"color":7,"editable":"yes","description":["x"]},
              {"id":"c","calendar_id":"primary","title":"Older","start":"2026-10-10","all_day":true}],
             "calendars":[
              {"id":"primary","name":"Personal","color":"#039be5","primary":true,"writable":true},
              {"id":"hol","name":"Holidays","color":"#0b8043","primary":false,"writable":false},
              {"name":"no id"}]}
            """
        guard let feed = try? JSONDecoder().decode(TimelineFeed.self, from: Data(body.utf8)) else {
            return expect(false, "the wave 3 timeline decodes")
        }
        let a = feed.events?.first { $0.id == "a" }
        expect(a?.color == "#039be5" && a?.editable == true && a?.description == "table for 4", "the new keys land")
        let b = feed.events?.first { $0.id == "b" }
        expect(b != nil && b?.color == nil && b?.editable == nil && b?.description == nil,
               "malformed new keys are nil, and the event is kept")
        let c = feed.events?.first { $0.id == "c" }
        expect(c != nil && c?.editable == nil && !CalendarWrite.canEdit(c), "an older event: not editable")
        expect(feed.calendars?.map(\.id) == ["primary", "hol"], "calendars decode; a row with no id is dropped")
        let old = try? JSONDecoder().decode(TimelineFeed.self, from: Data(#"{"ghosts":[],"events":[]}"#.utf8))
        expect(old != nil && old?.calendars == nil, "an older daemon: no calendars")
        let garbage = try? JSONDecoder().decode(
            TimelineFeed.self, from: Data(#"{"ghosts":[],"calendars":"nope"}"#.utf8))
        expect(garbage != nil && garbage?.calendars == nil, "a malformed calendars costs only itself")
        let body2 = try? JSONEncoder().encode(
            EventWrite(calendar_id: "primary", title: "x", start: "2026-10-08", end: "2026-10-08", all_day: true,
                       location: nil, description: nil))
        let keys = body2.flatMap { try? JSONSerialization.jsonObject(with: $0) as? [String: Any] }?.keys.sorted()
        expect(keys == ["all_day", "calendar_id", "end", "start", "title"],
               "a create body leaves an empty location and note out")
    }

    static func expect(_ cond: Bool, _ what: String) {
        checks += 1
        if !cond {
            failures += 1
            print("  FAIL: \(what)")
        }
    }
}

extension EventDraft {
    func withTitle(_ t: String) -> EventDraft {
        var d = self
        d.title = t
        return d
    }
}
