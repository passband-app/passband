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
        theTimesMenu()
        datesRead()
        aTimedDraftToTheWire()
        anAllDayDraftToTheWireWithTheInclusiveEnd()
        validation()
        summaries()
        anAgentsDraftDoesNotComeForwardOverAnEvent()
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
        aTimedOutWriteIsUncertainNotUnsaved()
        neverSentIsDecidedByEvidence()
        anUncertainCreateIsFoundByItsId()
        anUncertainEditLooksOnlyAtWhatItChanged()
        aDeleteThatCanNoLongerTellIsRefused()
        anAnswerAfterTheGateClosedIsNobodys()
        aTypedPaneSurvivesAPageChange()
        anUncertainSaveFreezesThePane()
        aPaneSendsItsOwnIdAndOnlyWhatItSent()
        anUncertainCreateIsFoundByItsIdAlone()
        anUncertainEditIsJudgedByWhatMoved()
        aResponseThatCameBackMayHaveArrived()
        each409SaysItsOwnThing()
        theVerdictKeysOnTheCode()
        anEditSettlesOnlyOnWhatWasTyped()
        aCreateFallbackIsTheUsersOwnNewEvent()
        theStoresPaneDecisions()
        theWave3WireDecodes()
        aMeetFollowsTheGuestsUntilSet()

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

    /// The wells' menus: a short run of half hours that holds the well's own
    /// time, and an end menu that counts from the start.
    static func theTimesMenu() {
        let morning = EventForm.startChoices("10am")
        expect(morning.count == 24, "twelve hours of half hours")
        expect(morning.first?.text == "8am", "the start menu opens two hours before 10am")
        let starts = EventForm.startChoices("7pm")
        expect(starts.first?.text == "12pm", "an evening start shifts the run back to stay inside the day")
        expect(starts.last?.text == "11:30pm", "and stops inside the day")
        expect(starts.first { $0.current }?.text == "7pm", "7pm is ticked")
        expect(EventForm.startChoices("11pm").first?.text == "12pm", "a late start shifts the run back, never past midnight")
        expect(EventForm.startChoices("1am").first?.text == "12am", "an early one starts at midnight")
        expect(EventForm.startChoices("").first?.text == "7am", "nothing readable centres on 9am")
        expect(!EventForm.startChoices("7:45pm").contains { $0.current }, "an off-grid time ticks nothing")
        expect(EventForm.parseTime(starts[13].text)?.minutes == 18 * 60 + 30, "what a pick writes reads back")

        let ends = EventForm.endChoices(start: "10pm", end: "11pm")
        expect(ends.first?.label == "10:30pm (30 min)", "the end menu starts half an hour after the start")
        expect(ends[1].label == "11pm (1 hr)" && ends[1].current, "and ticks the end it holds")
        expect(ends[2].label == "11:30pm (1.5 hrs)", "half hours read as halves")
        expect(ends[5].text == "1am", "past midnight is the small hours")
        expect(EventForm.parseEnd(ends[5].text, after: 22 * 60) == 60, "and reads back as the next morning")
        expect(EventForm.endChoices(start: "", end: "10am").first?.text == "8am", "no start: the start's own list")
        expect(EventForm.durationText(75) == "1 hr 15 min", "an odd length says its minutes")
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
        expect(problem(draft(start: "7pm", end: "7pm")) == nil,
               "the same time with no end day said is the next day's: a whole day")
        expect(problem(draft(start: "7pm", end: "7pm", endDate: "Oct 8")) == .noLength,
               "ending when it starts, on an end day said by hand")
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

    /// verify7 D5: an agent's draft asking for the screen (`open_draft`, a
    /// passband://draft link) against an event pane.
    static func anAgentsDraftDoesNotComeForwardOverAnEvent() {
        func held(_ exists: Bool, touched: Bool = false, parked: Bool = false, editable: Bool = true) -> Bool {
            EventPaneTravel.heldByUser(exists: exists, touched: touched, parked: parked, editable: editable)
        }
        expect(!held(false), "no pane: nothing is being written")
        expect(!held(false, touched: true, parked: true, editable: false), "whatever the leftovers say")
        expect(!held(true), "an untouched pane on screen gives way, as it does to any composer")
        expect(held(true, touched: true), "a pane with something typed in it is the user's writing")
        expect(held(true, parked: true), "so is a parked one, though it is not on screen")
        expect(held(true, editable: false), "and one whose write is out or frozen")
        expect(held(true, touched: true, parked: true, editable: false), "all three")
        expect(!EventPaneTravel.agentDraftMayComeForward(writing: true, composerHoldsIt: true),
               "nothing comes forward over something being written")
        expect(!EventPaneTravel.agentDraftMayComeForward(writing: false, composerHoldsIt: false),
               "nor for a draft the composer did not take: there is nothing to show")
        expect(!EventPaneTravel.agentDraftMayComeForward(writing: true, composerHoldsIt: false), "nor both")
        expect(EventPaneTravel.agentDraftMayComeForward(writing: false, composerHoldsIt: true),
               "only for a draft that is really in the composer, with nothing else being written")
    }

    static func summaries() {
        func s(_ d: EventDraft) -> String? {
            if case .success(let w) = EventForm.when(d, today: now, calendar: cal) {
                return EventForm.summary(w, now: now, calendar: cal)
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
        // THE YEAR, WHEN IT IS NOT THIS ONE, AND A START GONE BY (verify7
        // E13). Now is Mon Oct 5 2026, 3:40pm.
        func w(_ start: String, _ end: String, allDay: Bool = false) -> String {
            EventForm.summary(
                ResolvedWhen(allDay: allDay, start: allDay ? day(start) : at(start), end: allDay ? day(end) : at(end)),
                now: now, calendar: cal)
        }
        expect(w("2027-01-06T12:00:00-08:00", "2027-01-06T13:00:00-08:00") == "Wed Jan 6 2027, 12pm to 1pm",
               "next year is said")
        expect(w("2020-01-09T12:00:00-08:00", "2020-01-09T13:00:00-08:00") == "Thu Jan 9 2020, 12pm to 1pm, in the past",
               "a proposal for 2020 reads as 2020, and as gone")
        expect(w("2026-10-09T12:00:00-07:00", "2027-10-09T12:00:00-07:00") == "Fri Oct 9 2026, 12pm to 12pm Sat Oct 9 2027",
               "a year-long event says both years")
        expect(w("2026-12-31T22:00:00-08:00", "2027-01-01T02:00:00-08:00") == "Thu Dec 31, 10pm to 2am Fri",
               "new year's night is still a night")
        expect(w("2026-12-30", "2027-01-02", allDay: true) == "Wed Dec 30 2026 to Sat Jan 2 2027, all day",
               "a run of days over the year's end")
        expect(w("2027-03-04", "2027-03-04", allDay: true) == "Thu Mar 4 2027, all day", "an all-day next year")
        expect(w("2026-10-05T09:00:00-07:00", "2026-10-05T10:00:00-07:00") == "Mon Oct 5, 9am to 10am, in the past",
               "this morning has gone by")
        expect(w("2026-10-05T16:00:00-07:00", "2026-10-05T17:00:00-07:00") == "Mon Oct 5, 4pm to 5pm",
               "later today has not")
        expect(w("2026-10-01T09:00:00-07:00", "2026-10-01T10:00:00-07:00") == "Thu Oct 1, 9am to 10am, in the past",
               "last week, this year: no year, but said")
        expect(w("2026-10-05", "2026-10-05", allDay: true) == "Mon Oct 5, all day", "all day today has not gone by")
        expect(w("2026-10-04", "2026-10-04", allDay: true) == "Sun Oct 4, all day, in the past", "all day yesterday has")
        // A WEEKDAY IN THE END-DAY WELL IS READ FROM THE START (verify7 E16).
        expect(s(draft(date: "Oct 19", start: "7pm", end: "9pm", endDate: "tue")) == "Mon Oct 19, 7pm to 9pm Tue",
               "an event starting Mon Oct 19 that ends \"tue\" ends the Tuesday after it")
        expect(s(draft(date: "Oct 19", start: "7pm", end: "9pm", endDate: "mon")) == "Mon Oct 19, 7pm to 9pm",
               "the start's own weekday is the start day")
        expect(s(draft(date: "Oct 19", allDay: true, endDate: "friday")) == "Mon Oct 19 to Fri Oct 23, all day",
               "and the same for a last day")
        expect(s(draft(date: "Oct 19", start: "7pm", end: "9pm", endDate: "Oct 21")) == "Mon Oct 19, 7pm to 9pm Wed Oct 21",
               "a date is a date, whatever the start")
        var run = draft(date: "Oct 19", start: "7pm", end: "9pm", endDate: "wed")
        run = EventForm.moved(run, to: day("2026-10-26"), today: now, calendar: cal)
        expect(run.endDate == "Wed Oct 28", "a clicked day carries a weekday end day by its length from the start: \(run.endDate)")
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
        sweptDays()
    }

    /// Days swept on the month grid open an all-day run over them, either
    /// way round.
    static func sweptDays() {
        func run(_ a: String, _ b: String) -> EventDraft {
            EventForm.newDraft(from: day(a), through: day(b), now: now, calendarId: "primary", calendar: cal)
        }
        let three = run("2026-10-12", "2026-10-14")
        expect(three.allDay && three.date == "Mon Oct 12" && three.endDate == "Wed Oct 14",
               "an all-day run, first day through last")
        expect(run("2026-10-14", "2026-10-12") == three, "swept backwards, the same run")
        let w = wire(three.withTitle("x"))
        expect(w?.all_day == true && w?.start == "2026-10-12" && w?.end == "2026-10-14", "on the wire, the last day inclusive")
        expect(run("2026-10-12", "2026-10-12").endDate.isEmpty, "one day: the same day")
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
        expect(row(draft(date: "someday")) == nil && row(draft(start: "7pm", end: "sometime")) == nil,
               "while the times do not read there is no chip, not a wrong one")
        expect(row(draft(start: "7pm", end: "7pm"))?.end == at("2026-10-09T19:00:00-07:00"),
               "the same time twice is a whole day (wave 6), and the chip shows it")
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
        let all = ([nil] + CalendarRefusal.allCases.map { Optional($0) }).flatMap { refusal in
            [CalendarWriteAction.create, .edit, .delete].map { CalendarWrite.failureText(refusal, $0) }
        }
        expect(all.allSatisfy { !$0.isEmpty && !$0.contains("\u{2014}") }, "every refusal says something, no em dash")
        expect(CalendarWrite.failureText(.notFound, .edit) == "That event is no longer on your calendar.",
               "not_found on an edit: gone")
        expect(CalendarWrite.failureText(.googleRefused, .delete).contains("not deleted"), "a refused delete says so")
        expect(CalendarWrite.failureText(nil, .create).contains("nothing was saved"),
               "an unreachable server: nothing was saved")
        expect(CalendarWrite.failureText(nil, .delete).contains("not deleted"), "nor deleted")
        expect(Set(CalendarRefusal.allCases.map { CalendarWrite.failureText($0, .create) }).count
               == CalendarRefusal.allCases.count, "one sentence per code on a create")
    }

    /// verify4 X6: each 409 says its own thing, and none of them claims a
    /// deletion that did not happen.
    static func each409SaysItsOwnThing() {
        let inUse = CalendarWrite.failureText(.idInUse, .create)
        let deleted = CalendarWrite.failureText(.alreadyDeleted, .create)
        expect(inUse != deleted, "id in use and already deleted are two sentences")
        expect(!inUse.contains("deleted"), "id in use never says deleted")
        expect(deleted.contains("already added and then deleted"), "already deleted says so")
        expect(CalendarWrite.settlingText(.mayHaveHappened(code: "already_creating")) == CalendarWrite.stillSendingText,
               "already creating: still being sent, and the pane holds")
        expect(CalendarWrite.settlingText(.mayHaveHappened(code: "uncertain")) == CalendarWrite.uncertainText,
               "any other may-have: the usual line")
        for text in [inUse, deleted, CalendarWrite.stillSendingText] {
            expect(!text.contains("\u{2014}"), "no em dash: \(text)")
        }
    }

    /// A 504 (fix round 1, B5): Google was asked and the answer was lost, so
    /// it is not a refusal, and the pane closes only when the reloaded page
    /// shows the change.
    static func anUncertainWriteIsSettledByTheFeed() {
        let lost = APIError(.server, 504, "x", code: "uncertain")
        expect(CalendarWrite.verdict(of: lost) == .mayHaveHappened(code: "uncertain"), "a 504 may have happened")
        expect(CalendarWrite.settlingText(CalendarWrite.verdict(of: lost)) == "Google may have made this change. Refreshing…",
               "a 504 is never worded as a refusal")

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
               "create, no id: a new event with that title at that start on that calendar is")
        var later = made
        later.start = "2026-10-08T20:45:00-07:00"
        expect(CalendarWrite.made(body, before: had, in: TimelineFeed(events: [a, later])) == nil,
               "create, no id: the same title at another time is not it")
        let elsewhere = event("new", start: "2026-10-08T19:45:00-07:00", end: nil, calendar: "work")
        expect(CalendarWrite.made(body, before: had, in: TimelineFeed(events: [a, elsewhere])) == nil,
               "create: not on another calendar")
        expect(CalendarWrite.made(body, before: had, in: nil) == nil, "create: no feed proves nothing")

        let rename = EventPatch(title: "Renamed")
        expect(CalendarWrite.edited(a, rename, in: before) == nil, "edit: unchanged is not yet saved")
        var renamed = a
        renamed.title = "Renamed"
        expect(CalendarWrite.edited(a, rename, in: TimelineFeed(events: [renamed]))?.title == "Renamed",
               "edit: the event as the page now has it")
        expect(CalendarWrite.edited(a, rename, in: TimelineFeed(events: [])) == nil, "edit: gone is not this edit")

        expect(!CalendarWrite.gone(a, from: before), "delete: still there")
        expect(CalendarWrite.gone(a, from: TimelineFeed(events: [])), "delete: gone")
        let sameIdElsewhere = event("a", start: "2026-10-08T09:00:00-07:00", end: nil, calendar: "work")
        expect(CalendarWrite.gone(a, from: TimelineFeed(events: [sameIdElsewhere])),
               "delete: the same id on another calendar is another event")
        expect(!CalendarWrite.gone(a, from: nil) && !CalendarWrite.gone(a, from: TimelineFeed()),
               "delete: a feed that never loaded proves nothing")
    }

    /// verify2 V1: a calendar write the app gave up waiting on (or whose
    /// connection dropped mid-request) may have reached the daemon, which
    /// goes on without the app. It is the 504 "may have happened", settled
    /// against the page, never "could not reach your server, so nothing was
    /// saved". A request that never got there still is.
    static func aTimedOutWriteIsUncertainNotUnsaved() {
        for code in [URLError.Code.timedOut, .networkConnectionLost] {
            let e = APIError.transport(URLError(code))
            expect(e.kind == .network && e.mayHaveArrived, "\(code): may have arrived")
            expect(CalendarWrite.verdict(of: e) == .mayHaveHappened(code: nil), "\(code): settled, may have happened")
        }
        for code in [URLError.Code.cannotConnectToHost, .cannotFindHost, .dnsLookupFailed,
                     .secureConnectionFailed] {
            let e = APIError.transport(URLError(code))
            expect(!e.mayHaveArrived && CalendarWrite.verdict(of: e) == .notSent, "\(code): never got there")
        }
        // verify6 Z1: "not connected" is also what an interface dropping
        // under a LIVE request reads as, so the code alone proves nothing.
        let offline = APIError.transport(URLError(.notConnectedToInternet))
        expect(offline.mayHaveArrived && CalendarWrite.verdict(of: offline).mayHaveHappened,
               "notConnectedToInternet with no evidence: may have arrived")
        expect(!APIError.notSentCodes.contains(.notConnectedToInternet), "and it is off the unsent list")
        // verify5 Y2: when in doubt, freeze. Anything not provably before the
        // send (a cancellation, an unknown code, a decode failure, an error
        // that is not a URLError) may have arrived.
        for code in [URLError.Code.cancelled, .unknown, .cannotDecodeContentData, .dataLengthExceedsMaximum,
                     .resourceUnavailable] {
            let e = APIError.transport(URLError(code))
            expect(e.mayHaveArrived && CalendarWrite.verdict(of: e).mayHaveHappened, "\(code): may have arrived")
        }
        struct Odd: Error {}
        expect(APIError.transport(Odd()).mayHaveArrived, "an error that is not a URLError may have arrived")
        expect(CalendarWrite.verdict(of: APIError(.network, 0, "client not configured")) == .notSent,
               "a request never built is unsent")
        expect(CalendarWrite.verdict(of: URLError(.badURL)) == .mayHaveHappened(code: nil),
               "not an APIError: may have happened")
    }

    /// verify4 X1: UNLOCK ONLY ON A DEFINITE "NOTHING HAPPENED". The verdict
    /// keys on the daemon's code, and the code must come with its own
    /// status. Everything else freezes the pane.
    static func theVerdictKeysOnTheCode() {
        let statuses: [String: Int] = [
            "invalid": 400, "unknown_calendar": 400, "read_only": 400, "cannot_invite": 400,
            "guests_read_only": 400, "cannot_notify": 400, "no_meet": 400, "has_meeting": 400, "not_granted": 403, "not_editable": 403, "not_invited": 403,
            "not_found": 404, "id_in_use": 409, "already_deleted": 409, "calendar_gone": 410,
            "google_refused": 502,
        ]
        expect(Set(CalendarRefusal.allCases.map(\.rawValue)) == Set(statuses.keys), "the closed list, whole")
        for refusal in CalendarRefusal.allCases {
            let status = statuses[refusal.rawValue] ?? -1
            expect(refusal.status == status, "\(refusal.rawValue) comes with \(status)")
            let e = APIError(APIErrorKind.forStatus(status), status, "msg", code: refusal.rawValue)
            expect(CalendarWrite.verdict(of: e) == .refused(refusal), "\(refusal.rawValue): nothing happened")
            expect(EventPaneRules.failureEvent(CalendarWrite.verdict(of: e)) == .refused,
                   "\(refusal.rawValue): the pane is the user's again")
            // The same code with any other status is not that refusal.
            for other in [200, 400, 403, 404, 409, 410, 500, 502, 504] where other != status {
                let odd = APIError(APIErrorKind.forStatus(other), other, "msg", code: refusal.rawValue)
                expect(CalendarWrite.verdict(of: odd).mayHaveHappened,
                       "\(refusal.rawValue) with \(other): may have happened")
            }
        }
        // May have happened: every status without a code, and the daemon's
        // own may-have codes.
        for status in [400, 403, 404, 409, 410, 500, 502, 503, 504] {
            let bare = APIError(APIErrorKind.forStatus(status), status, "request failed (\(status))")
            expect(CalendarWrite.verdict(of: bare) == .mayHaveHappened(code: nil), "\(status) with no code: freeze")
        }
        let unsure: [(String, Int)] = [("uncertain", 504), ("internal", 500), ("already_creating", 409),
                                       ("something_new", 400), ("google_refused_later", 502), ("", 400)]
        for (code, status) in unsure {
            let e = APIError(APIErrorKind.forStatus(status), status, "msg", code: code)
            expect(CalendarWrite.verdict(of: e) == .mayHaveHappened(code: code), "\(code) \(status): freeze")
            expect(EventPaneRules.failureEvent(CalendarWrite.verdict(of: e)) == nil, "\(code): settled, not refused")
        }
        expect(CalendarRefusal(rawValue: CalendarRefusal.alreadyCreatingCode) == nil
               && CalendarRefusal(rawValue: CalendarRefusal.uncertainCode) == nil
               && CalendarRefusal(rawValue: CalendarRefusal.internalCode) == nil,
               "the daemon's may-have codes are not refusals")
        // A 2xx the app could not decode, and a proxy's own page.
        expect(CalendarWrite.verdict(of: APIError(.unknown, 0, "unexpected response shape")).mayHaveHappened,
               "an answer that would not decode: freeze")
        expect(CalendarWrite.verdict(of: APIError(.server, 502, "request failed (502)")).mayHaveHappened,
               "a bare 502: freeze")
        expect(CalendarWrite.verdict(of: APIError(.server, 500, "internal error")).mayHaveHappened,
               "a 500: freeze")
        var something = APIError(.network, 0, "cannot reach squelch server")
        something.mayHaveArrived = true
        expect(CalendarWrite.verdict(of: something).mayHaveHappened, "a response of no known kind: freeze")
    }

    /// verify2 V2: the app names the new event's id, and an uncertain create
    /// is found by that id whatever the title came back as (laundered,
    /// collapsed, a URL cut down); a second press of the same pane sends the
    /// same id.
    static func anUncertainCreateIsFoundByItsId() {
        let id = CalendarWrite.newEventId()
        let base32hex = Set("0123456789abcdefghijklmnopqrstuv")
        expect((5...1024).contains(id.count) && id.allSatisfy { base32hex.contains($0) },
               "the id is one Google takes")
        expect(CalendarWrite.newEventId() != id, "a new pane, a new id")
        var body = EventWrite(
            calendar_id: "primary", title: "Lunch  with\u{200B} Sam https://x.test/a?pwd=1",
            start: "2026-10-08T12:00:00-07:00", end: "2026-10-08T13:00:00-07:00", all_day: false,
            location: nil, description: nil)
        body.id = id
        let json = String(data: try! JSONEncoder().encode(body), encoding: .utf8)!
        expect(json.contains("\"id\":\"\(id)\""), "the id is on the wire")
        var stored = event(id, title: "Lunch with Sam x.test/a", start: body.start, end: body.end)
        expect(CalendarWrite.made(body, before: [], in: TimelineFeed(events: [stored]))?.id == id,
               "found by id though the title came back laundered")
        stored.calendar_id = "work"
        expect(CalendarWrite.made(body, before: [], in: TimelineFeed(events: [stored])) == nil,
               "the id on another calendar is another event")
        let lookalike = event("other", title: body.title, start: body.start, end: body.end)
        expect(CalendarWrite.made(body, before: [CalendarWrite.key(lookalike)], in: TimelineFeed(events: [lookalike]))
               == nil, "with an id, a lookalike already there before the send is not it")
        var plain = body
        plain.id = nil
        let laundered = event("g1", title: "Lunch with Sam https://x.test/a?pwd=1", start: body.start, end: body.end)
        expect(CalendarWrite.made(plain, before: [], in: TimelineFeed(events: [laundered]))?.id == "g1",
               "no id: the title compares laundered")
        expect(CalendarWrite.failureText(.alreadyDeleted, .create).contains("already added"),
               "a spent id says the event was made and deleted")
    }

    /// verify2 V3: an uncertain edit is found only when every field it
    /// changed reads as typed. A guest answering, a colour changing, or any
    /// field it did not touch is never taken for it.
    static func anUncertainEditLooksOnlyAtWhatItChanged() {
        let a = event("a", start: "2026-10-08T09:00:00-07:00", end: "2026-10-08T10:00:00-07:00",
                      location: "Kitchen", description: "Old")
        // Somebody else's change landed; this edit did not.
        var other = a
        other.color = "#ff0000"
        other.guests = [TimelineGuest(email: "sam@x.test", name: nil, response: .accepted, organizer: nil, isSelf: nil)]
        let rename = EventPatch(title: "Lunch  with Sam")
        expect(CalendarWrite.edited(a, rename, in: TimelineFeed(events: [other])) == nil,
               "another change is not this edit")
        var done = other
        done.title = "Lunch with Sam"
        expect(CalendarWrite.edited(a, rename, in: TimelineFeed(events: [done])) != nil,
               "the title, laundered, is this edit")

        let move = EventPatch(start: "2026-10-08T11:00:00-07:00", end: "2026-10-08T12:00:00-07:00", all_day: false)
        var moved = a
        moved.start = "2026-10-08T18:00:00Z"
        moved.end = "2026-10-08T19:00:00Z"
        expect(CalendarWrite.edited(a, move, in: TimelineFeed(events: [moved])) != nil,
               "times compare as instants, not strings")
        moved.end = "2026-10-08T19:30:00Z"
        expect(CalendarWrite.edited(a, move, in: TimelineFeed(events: [moved])) == nil, "a different end is not it")

        let place = EventPatch(location: .set("Zoom https://www.zoom.us/j/1?pwd=abc"), description: .clear)
        var placed = a
        placed.location = "Zoom zoom.us/j/1"
        expect(CalendarWrite.edited(a, place, in: TimelineFeed(events: [placed])) == nil,
               "a cleared note still there is not it")
        placed.description = nil
        expect(CalendarWrite.edited(a, place, in: TimelineFeed(events: [placed])) == nil,
               "verify4 X2: a link cut to host and path is not what was typed: unknown, the pane stays frozen")
        placed.location = "Zoom  https://www.zoom.us/j/1?pwd=abc"
        expect(CalendarWrite.edited(a, place, in: TimelineFeed(events: [placed])) != nil,
               "the place as typed, whitespace aside, and the note cleared: this edit")

        let invite = EventPatch(guests: ["Sam@x.test"])
        var invited = a
        invited.guests = [
            TimelineGuest(email: "me@x.test", name: nil, response: nil, organizer: true, isSelf: true),
            TimelineGuest(email: "sam@x.test", name: nil, response: .pending, organizer: nil, isSelf: nil),
        ]
        expect(CalendarWrite.edited(a, invite, in: TimelineFeed(events: [invited])) != nil,
               "guests compare as addresses, the user left out")
        expect(CalendarWrite.edited(a, invite, in: TimelineFeed(events: [a])) == nil, "no guest yet is not it")

        let oneDay = event("d", start: "2026-10-08", end: nil, allDay: true)
        let toOneDay = EventPatch(start: "2026-10-09", end: "2026-10-09", all_day: true)
        var shifted = oneDay
        shifted.start = "2026-10-09"
        expect(CalendarWrite.edited(oneDay, toOneDay, in: TimelineFeed(events: [shifted])) != nil,
               "a one-day event is served with no end")
    }

    /// verify2 V5: "delete and tell guests" on a calendar that may no longer
    /// tell them deletes nothing, as a save refuses; telling was never asked,
    /// or there is nobody to tell, deletes silently.
    static func aDeleteThatCanNoLongerTellIsRefused() {
        expect(CalendarWrite.deleteNotify(tellGuests: true, hasGuests: true, mayTell: false) == nil,
               "asked to tell, may not: refused")
        expect(CalendarWrite.deleteNotify(tellGuests: true, hasGuests: true, mayTell: true) == true, "told")
        expect(CalendarWrite.deleteNotify(tellGuests: false, hasGuests: true, mayTell: false) == false,
               "never asked: silent")
        expect(CalendarWrite.deleteNotify(tellGuests: true, hasGuests: false, mayTell: false) == false,
               "nobody to tell: silent")
        expect(!CalendarWrite.cannotTellText.contains("\u{2014}") && CalendarWrite.cannotTellText.contains("not deleted"),
               "the refusal says nothing was deleted")
        expect(CalendarWrite.failureText(.calendarGone, .delete).contains("nothing was deleted"),
               "a lost calendar is not 'deleted'")
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

    // MARK: fix round 3: the frozen pane

    /// THE FROZEN PANE, the whole table: every state, every event, every key.
    /// An uncertain save that the page cannot confirm freezes the pane; the
    /// only ways on are check again, send again (the same body) and close;
    /// and nothing ever leads from a frozen pane back to an editable one.
    static func anUncertainSaveFreezesThePane() {
        typealias S = EventPaneState
        let states: [S] = [.editing, .sending(again: false), .sending(again: true), .uncertain, .confirmed]
        let events: [S.Event] = [.send, .answered, .refused, .unsure, .checkAgain, .sendAgain]
        let table: [S: [S.Event: S]] = [
            .editing: [.send: .sending(again: false)],
            .sending(again: false): [.answered: .confirmed, .refused: .editing, .unsure: .uncertain],
            .sending(again: true): [.answered: .confirmed, .refused: .uncertain, .unsure: .uncertain],
            .uncertain: [.checkAgain: .sending(again: true), .sendAgain: .sending(again: true)],
            .confirmed: [:],
        ]
        for state in states {
            for event in events {
                let want = table[state]?[event] ?? state
                expect(state.next(event) == want, "\(state) + \(event) is \(want)")
            }
        }
        // No way back: from a frozen pane, any run of events never reaches
        // an editable one.
        var seen: Set<S> = [.uncertain]
        var frontier: [S] = [.uncertain]
        while let s = frontier.popLast() {
            for e in events where !seen.contains(s.next(e)) {
                seen.insert(s.next(e))
                frontier.append(s.next(e))
            }
        }
        expect(!seen.contains(.editing) && !seen.contains { $0.fieldsEditable },
               "nothing leads from uncertain back to editable")
        expect(seen == [.uncertain, .sending(again: true), .confirmed], "frozen, checking, or done")

        expect(states.filter(\.fieldsEditable) == [.editing], "fields take typing only while editing")
        expect(states.filter(\.frozen) == [.sending(again: true), .uncertain], "the frozen footer")
        expect(S.uncertain.frozenControls == [.checkAgain, .sendAgain, .close], "frozen: three ways on")
        expect(S.sending(again: true).frozenControls == [.close], "checking: only close")
        expect(S.editing.frozenControls.isEmpty && S.sending(again: false).frozenControls.isEmpty,
               "an editable pane has its save controls instead")

        let keys: [(S, S.Key, S.KeyAction)] = [
            (.editing, .enter, .editingRules), (.editing, .commandEnter, .editingRules), (.editing, .escape, .close),
            (.sending(again: false), .enter, .nothing), (.sending(again: false), .commandEnter, .nothing),
            (.sending(again: false), .escape, .close),
            (.uncertain, .enter, .nothing), (.uncertain, .commandEnter, .checkAgain), (.uncertain, .escape, .close),
            (.sending(again: true), .enter, .nothing), (.sending(again: true), .commandEnter, .nothing),
            (.sending(again: true), .escape, .close),
        ]
        for (state, key, want) in keys {
            expect(state.action(key) == want, "\(state): \(key) is \(want)")
        }

        let body = EventWrite(
            calendar_id: "primary", title: "x", start: "2026-10-08T19:45:00-07:00",
            end: "2026-10-08T21:00:00-07:00", all_day: false, location: nil, description: nil,
            guests: ["sam@x.test"])
        let a = event("a", start: "2026-10-08T09:00:00-07:00", end: "2026-10-08T10:00:00-07:00")
        let invite = CalendarWrite.createWrite(body, notify: true, createId: "abcde12345", before: [])
        expect(EventPaneState.sendAgainLabel(invite) == "send again", "a create's id keeps it to one invitation")
        let tell = SentWrite.edit(a, EventPatch(title: "y"), notify: true)
        expect(EventPaneState.sendAgainLabel(tell) == "send again, guests may be told twice",
               "an edit that tells guests says it may tell them twice")
        expect(EventPaneState.sendAgainLabel(.edit(a, EventPatch(title: "y"), notify: false)) == "send again",
               "an edit that tells nobody")
        for text in [CalendarWrite.frozenText, CalendarWrite.frozenCheckingText, CalendarWrite.resendRefusedText,
                     EventPaneState.sendAgainLabel(tell), EventPaneState.checkAgainLabel, EventPaneState.closeLabel] {
            expect(!text.contains("\u{2014}"), "no em dash: \(text)")
        }
        expect(CalendarWrite.frozenText == "Google may have made this change. Passband could not confirm it yet.",
               "the frozen line")
    }

    /// verify3 W6: the bodies a pane sends, built in one pure place. A
    /// create carries the PANE'S id (the same on every send from it, a new
    /// one for a new pane); a create is what "send again" resends whole; an
    /// edit that changes nothing sends nothing; a delete that can no longer
    /// tell is refused.
    static func aPaneSendsItsOwnIdAndOnlyWhatItSent() {
        let made = EventWrite(
            calendar_id: "primary", title: "Dinner", start: "2026-10-08T19:45:00-07:00",
            end: "2026-10-08T21:00:00-07:00", all_day: false, location: nil, description: nil,
            guests: ["sam@x.test"])
        let paneId = CalendarWrite.newEventId()
        let first = CalendarWrite.createWrite(made, notify: true, createId: paneId, before: ["primary\nold"])
        let second = CalendarWrite.createWrite(made, notify: true, createId: paneId, before: ["primary\nold"])
        guard case .create(let body, let before) = first else { return expect(false, "a create") }
        expect(body.id == paneId, "the pane's id is on the body")
        expect(body.notify == true && first.tellsGuests, "the control's word is on the body")
        expect(before == ["primary\nold"], "what the page held when it was sent")
        expect(first == second, "two sends from one pane are one body, one id")
        let otherPane = CalendarWrite.createWrite(
            made, notify: true, createId: CalendarWrite.newEventId(), before: ["primary\nold"])
        expect(otherPane != first, "a new pane, a new id")
        guard case .create(let quiet, _) = CalendarWrite.createWrite(made, notify: false, createId: paneId, before: [])
        else { return expect(false, "a create") }
        expect(quiet.notify == nil && quiet.id == paneId, "telling nobody sends no notify key, the id still")

        let a = event("a", start: "2026-10-08T09:00:00-07:00", end: "2026-10-08T10:00:00-07:00")
        expect(CalendarWrite.editWrite(a, EventPatch(), notify: true) == nil, "an empty edit sends nothing")
        expect(CalendarWrite.editWrite(a, EventPatch(title: "x"), notify: true)
               == .edit(a, EventPatch(title: "x"), notify: true), "an edit as sent")

        expect(CalendarWrite.deleteWrite(a, tellGuests: true, hasGuests: true, mayTell: false) == nil,
               "asked to tell, may not: no delete is sent")
        let told = CalendarWrite.deleteWrite(a, tellGuests: true, hasGuests: true, mayTell: true)
        expect(told?.notify == true && told?.calendarId == "primary" && told?.id == "a", "a delete that tells")
        expect(CalendarWrite.deleteWrite(a, tellGuests: false, hasGuests: true, mayTell: true)?.notify == false,
               "a delete that tells nobody")
    }

    /// verify5 Y3 (replacing verify3 W3's fallback): a create sent with an
    /// id is found by that id ALONE. A lookalike new since the send, even
    /// the user's own from another pane, is not it; an older daemon that
    /// ignored the id leaves the pane frozen until it is closed by hand.
    static func anUncertainCreateIsFoundByItsIdAlone() {
        var body = EventWrite(
            calendar_id: "primary", title: "Lunch with Sam", start: "2026-10-08T12:00:00-07:00",
            end: "2026-10-08T13:00:00-07:00", all_day: false, location: nil, description: nil)
        body.id = "abcdef0123456789"
        let lookalike = event("g9", title: "Lunch with Sam", start: body.start, end: body.end)
        expect(CalendarWrite.made(body, before: [], in: TimelineFeed(events: [lookalike])) == nil,
               "no event has the id: a lookalike is not it")
        let ours = event(body.id!, title: "Lunch with Sam", start: body.start, end: body.end)
        expect(CalendarWrite.made(body, before: [], in: TimelineFeed(events: [lookalike, ours]))?.id == body.id,
               "the event with the id is it")
        var elsewhere = ours
        elsewhere.calendar_id = "team"
        expect(CalendarWrite.made(body, before: [], in: TimelineFeed(events: [elsewhere])) == nil,
               "the id on another calendar is not it")
        let sent = SentWrite.create(body, before: [])
        expect(sent.found(in: TimelineFeed(events: [lookalike])) == nil, "a sent create looks the same way")
        expect(sent.found(in: TimelineFeed(events: [ours]))?.id == body.id, "and finds its own")
    }

    /// verify3 W4, W5: an uncertain edit is judged by whether each changed
    /// field MOVED from the original, not by copying the daemon's cleaning.
    static func anUncertainEditIsJudgedByWhatMoved() {
        let a = event("a", start: "2026-10-08T09:00:00-07:00", end: "2026-10-08T10:00:00-07:00",
                      location: "zoom.us/j/1", description: "Old")
        // W4, as verify4 X2 left it: the daemon cuts a link the app would
        // not have. The served text is not what was typed, so it is NOT
        // confirmed: the pane stays frozen and is closed by hand. Unknown,
        // never a false success.
        let link = EventPatch(location: .set("Zoom:https://zoom.us/j/2?pwd=abc"))
        var applied = a
        applied.location = "Zoom:zoom.us/j/2"
        expect(CalendarWrite.edited(a, link, in: TimelineFeed(events: [applied])) == nil,
               "W4/X2: a link cleaned differently is unknown, not confirmed")
        let notes = EventPatch(description: .set("a <b>bold</b> &amp; c"))
        var noted = a
        noted.description = "a bold & c"
        expect(CalendarWrite.edited(a, notes, in: TimelineFeed(events: [noted])) == nil,
               "W4/X2: notes with markup, cleaned, are unknown")
        noted.description = "  a <b>bold</b>\u{200B} &amp;   c "
        expect(CalendarWrite.edited(a, notes, in: TimelineFeed(events: [noted])) != nil,
               "notes as typed, whitespace aside, are this edit")
        // W5: a change that vanishes under cleaning (only the passcode) is
        // never claimed, applied or not.
        let passcode = EventPatch(location: .set("https://zoom.us/j/1?pwd=NEW"))
        expect(CalendarWrite.edited(a, passcode, in: TimelineFeed(events: [a])) == nil,
               "W5: a change that cannot be seen is not confirmed")
        // A title that did not move is not this edit, whatever was typed.
        expect(CalendarWrite.edited(a, EventPatch(title: "Dreamers  Dinner"), in: TimelineFeed(events: [a])) == nil,
               "a title still as it was is not confirmed")
        // Times sent as a pair: an unchanged start is not asked to move.
        let later = EventPatch(start: "2026-10-08T09:00:00-07:00", end: "2026-10-08T11:00:00-07:00")
        var stretched = a
        stretched.end = "2026-10-08T18:00:00Z"
        expect(CalendarWrite.edited(a, later, in: TimelineFeed(events: [stretched])) != nil,
               "only the end moved, and it reads as sent")
        expect(CalendarWrite.edited(a, later, in: TimelineFeed(events: [a])) == nil, "the end not moved yet")
        // Nothing the patch names can move: nothing confirms it.
        let same = EventPatch(start: "2026-10-08T16:00:00Z", end: "2026-10-08T17:00:00Z")
        expect(CalendarWrite.edited(a, same, in: TimelineFeed(events: [a])) == nil,
               "an edit with nothing observable is never confirmed")
        // A clear of something already empty is no change; of something
        // there, it must be gone.
        let clear = EventPatch(location: .clear)
        var cleared = a
        cleared.location = nil
        expect(CalendarWrite.edited(a, clear, in: TimelineFeed(events: [cleared])) != nil, "cleared")
        expect(CalendarWrite.edited(a, clear, in: TimelineFeed(events: [a])) == nil, "not cleared yet")
    }

    /// verify6 Z1: "never sent" is decided by what the task itself recorded,
    /// and by the error's code only when it recorded nothing.
    static func neverSentIsDecidedByEvidence() {
        func sent(_ code: URLError.Code?, _ e: SendEvidence?) -> Bool { !APIError.neverSent(code: code, evidence: e) }
        let started = SendEvidence(requestStarted: true, bodyBytesSent: 0, hadBody: true)
        let neverStarted = SendEvidence(requestStarted: false, bodyBytesSent: 0, hadBody: true)
        let noMetricsNoBytes = SendEvidence(requestStarted: nil, bodyBytesSent: 0, hadBody: true)
        let noMetricsBytes = SendEvidence(requestStarted: nil, bodyBytesSent: 212, hadBody: true)
        let bodylessUnknown = SendEvidence(requestStarted: nil, bodyBytesSent: 0, hadBody: false)
        let nothing = SendEvidence(requestStarted: nil, bodyBytesSent: nil, hadBody: true)
        // 1. Evidence of a send wins over any code, the "unsent" ones too.
        for code in [URLError.Code.notConnectedToInternet, .cannotConnectToHost, .secureConnectionFailed, .timedOut] {
            expect(sent(code, started), "\(code) after the request started: may have arrived")
            expect(sent(code, noMetricsBytes), "\(code) with body bytes sent: may have arrived")
        }
        expect(sent(.notConnectedToInternet, SendEvidence(requestStarted: false, bodyBytesSent: 1, hadBody: true)),
               "bytes sent outvote metrics that saw no request")
        // 2. Metrics that saw no request start are proof, whatever the code:
        // a true offline save keeps its friendly message.
        for code in [URLError.Code.notConnectedToInternet, .timedOut, .networkConnectionLost, .cancelled, .unknown] {
            expect(!sent(code, neverStarted), "\(code) with no request ever started: never sent")
        }
        expect(!sent(nil, neverStarted), "and an error that is not a URLError")
        expect(!sent(.notConnectedToInternet, SendEvidence(requestStarted: false, bodyBytesSent: nil, hadBody: false)),
               "a bodyless request (a delete) is proved by its metrics alone")
        // 3. No metrics: a body of which nothing went is proof; a bodyless
        // request proves nothing by its zero.
        expect(!sent(.notConnectedToInternet, noMetricsNoBytes), "a body, none of it sent: never sent")
        expect(!sent(.timedOut, noMetricsNoBytes), "whatever the code")
        expect(sent(.notConnectedToInternet, bodylessUnknown), "a delete's zero bytes prove nothing: may have arrived")
        expect(sent(.timedOut, bodylessUnknown), "nor for a timeout")
        // 4. Nothing recorded: the code list, as with no evidence at all.
        expect(sent(.notConnectedToInternet, nothing) && sent(.notConnectedToInternet, nil),
               "no evidence: notConnectedToInternet may have arrived")
        expect(sent(.timedOut, nothing) && sent(nil, nothing) && sent(nil, nil), "and so may anything unlisted")
        expect(!sent(.cannotFindHost, nothing) && !sent(.cannotFindHost, nil) && !sent(.cannotConnectToHost, bodylessUnknown),
               "no evidence: the provably-before codes are still unsent")
        // Through the door the client uses, to the pane's verdict.
        let live = APIError.transport(URLError(.notConnectedToInternet), evidence: started)
        expect(CalendarWrite.verdict(of: live).mayHaveHappened, "an interface dropped mid-request freezes the pane")
        let dead = APIError.transport(URLError(.notConnectedToInternet), evidence: neverStarted)
        expect(CalendarWrite.verdict(of: dead) == .notSent, "a save while truly offline is unsent, and says so")
    }

    /// verify3 W10: an answer that came back but could not be read means
    /// the request reached the server, so a calendar write may have
    /// happened.
    static func aResponseThatCameBackMayHaveArrived() {
        for code in [URLError.Code.badServerResponse, .cannotParseResponse, .zeroByteResource] {
            let e = APIError.transport(URLError(code))
            expect(e.mayHaveArrived && CalendarWrite.verdict(of: e).mayHaveHappened,
                   "\(code): may have arrived")
        }
    }

    /// verify4 X2: a text field the edit changed counts only when it reads
    /// as typed. The reviewer's three cases.
    static func anEditSettlesOnlyOnWhatWasTyped() {
        let a = event("a", title: "Dinner", start: "2026-10-08T09:00:00-07:00", end: "2026-10-08T10:00:00-07:00",
                      location: "Kitchen", description: "Old")
        // A) retitled Dinner -> Supper; a co-editor's "Lunch" landed instead.
        var lunch = a
        lunch.title = "Lunch"
        expect(CalendarWrite.edited(a, EventPatch(title: "Supper"), in: TimelineFeed(events: [lunch])) == nil,
               "A: the title moved to something else: unknown")
        var supper = a
        supper.title = "  Supper "
        expect(CalendarWrite.edited(a, EventPatch(title: "Supper"), in: TimelineFeed(events: [supper])) != nil,
               "A: the title as typed, whitespace aside: confirmed")
        // B) two uncertain edits: pane 1 (A -> B) frozen and closed, pane 2
        // opened on the stale page (still A) typed C; the page now shows B.
        var first = a
        first.title = "B"
        expect(CalendarWrite.edited(a, EventPatch(title: "C"), in: TimelineFeed(events: [first])) == nil,
               "B: the first edit landing is not the second")
        // C) the same for the place.
        var elsewhere = a
        elsewhere.location = "Garden"
        expect(CalendarWrite.edited(a, EventPatch(location: .set("Porch")), in: TimelineFeed(events: [elsewhere])) == nil,
               "C: the place moved elsewhere: unknown")
        var porch = a
        porch.location = "Porch"
        expect(CalendarWrite.edited(a, EventPatch(location: .set("Porch")), in: TimelineFeed(events: [porch])) != nil,
               "C: the place as typed: confirmed")
        // A title edit and a time edit together: both must read as sent.
        let both = EventPatch(title: "Supper", start: "2026-10-08T11:00:00-07:00", end: "2026-10-08T12:00:00-07:00")
        var half = a
        half.title = "Supper"
        expect(CalendarWrite.edited(a, both, in: TimelineFeed(events: [half])) == nil, "the times not moved yet")
        half.start = "2026-10-08T11:00:00-07:00"
        half.end = "2026-10-08T12:00:00-07:00"
        expect(CalendarWrite.edited(a, both, in: TimelineFeed(events: [half])) != nil, "both as sent")
    }

    /// verify4 X3: a create body WITHOUT an id (none is sent today; the rule
    /// stays for one) takes only the user's own new event at that calendar,
    /// title, start AND end.
    static func aCreateFallbackIsTheUsersOwnNewEvent() {
        let body = EventWrite(
            calendar_id: "primary", title: "Dinner", start: "2026-10-08T19:00:00-07:00",
            end: "2026-10-08T21:00:00-07:00", all_day: false, location: nil, description: nil)
        // Sam's invitation: same calendar, title and start, new since the
        // send, but Sam's (not editable here, Sam the organizer).
        var sams = event("sam1", title: "Dinner", start: body.start, end: body.end, editable: false)
        sams.organizer = "sam@x.test"
        sams.guests = [
            TimelineGuest(email: "sam@x.test", name: nil, response: .accepted, organizer: true, isSelf: nil),
            TimelineGuest(email: "me@x.test", name: nil, response: .pending, organizer: nil, isSelf: true),
        ]
        expect(CalendarWrite.made(body, before: [], in: TimelineFeed(events: [sams])) == nil,
               "someone else's Dinner at the same start is not it")
        var samsEditable = sams
        samsEditable.editable = true
        expect(CalendarWrite.made(body, before: [], in: TimelineFeed(events: [samsEditable])) == nil,
               "nor when a guest is named as another organizer")
        let short = event("g1", title: "Dinner", start: body.start, end: "2026-10-08T20:00:00-07:00")
        expect(CalendarWrite.made(body, before: [], in: TimelineFeed(events: [short])) == nil,
               "another end is not it")
        let mine = event("g2", title: "Dinner", start: body.start, end: "2026-10-09T04:00:00Z")
        expect(CalendarWrite.made(body, before: [], in: TimelineFeed(events: [sams, mine]))?.id == "g2",
               "the user's own, at that start and end: it")
        expect(CalendarWrite.made(body, before: [CalendarWrite.key(mine)], in: TimelineFeed(events: [mine])) == nil,
               "there before the send: not it")
        let day = EventWrite(
            calendar_id: "primary", title: "Trip", start: "2026-10-08", end: "2026-10-08", all_day: true,
            location: nil, description: nil)
        let oneDay = event("g3", title: "Trip", start: "2026-10-08", end: nil, allDay: true)
        expect(CalendarWrite.made(day, before: [], in: TimelineFeed(events: [oneDay]))?.id == "g3",
               "a one-day all-day event is served with no end")
        let timed = event("g4", title: "Trip", start: "2026-10-08", end: "2026-10-08")
        expect(CalendarWrite.made(day, before: [], in: TimelineFeed(events: [timed])) == nil,
               "not all-day: not it")
    }

    /// verify4 X5: the store's pane decisions, pure.
    static func theStoresPaneDecisions() {
        typealias P = EventPaneRules.Pane
        let open = P(isOpen: true, sameAccount: true, writing: false, state: .editing)
        expect(EventPaneRules.maySave(open), "the open, editable pane saves")
        for state in [EventPaneState.sending(again: false), .sending(again: true), .uncertain, .confirmed] {
            var p = open
            p.state = state
            expect(!EventPaneRules.maySave(p), "\(state): saveEvent does not run")
        }
        for broken in [P(isOpen: false, sameAccount: true, writing: false, state: .editing),
                       P(isOpen: true, sameAccount: false, writing: false, state: .editing),
                       P(isOpen: true, sameAccount: true, writing: true, state: .editing)] {
            expect(!EventPaneRules.maySave(broken), "\(broken): saveEvent does not run")
        }

        let a = event("a", start: "2026-10-08T09:00:00-07:00", end: "2026-10-08T10:00:00-07:00")
        let sent = SentWrite.edit(a, EventPatch(title: "Sent"), notify: true)
        let frozen = P(isOpen: true, sameAccount: true, writing: false, state: .uncertain)
        expect(EventPaneRules.again(frozen, sent: sent) == sent, "send again sends exactly what was sent")
        expect(EventPaneRules.again(frozen, sent: nil) == nil, "nothing sent: nothing to send again")
        for state in [EventPaneState.editing, .sending(again: false), .sending(again: true), .confirmed] {
            var p = frozen
            p.state = state
            expect(EventPaneRules.again(p, sent: sent) == nil, "\(state): no send again, no check again")
        }
        for broken in [P(isOpen: false, sameAccount: true, writing: false, state: .uncertain),
                       P(isOpen: true, sameAccount: false, writing: false, state: .uncertain),
                       P(isOpen: true, sameAccount: true, writing: true, state: .uncertain)] {
            expect(EventPaneRules.again(broken, sent: sent) == nil, "\(broken): no send again")
        }

        let mine = UUID(), other = UUID()
        expect(EventPaneRules.applies(open: mine, sentFrom: mine), "the pane that sent it: the answer applies")
        expect(!EventPaneRules.applies(open: other, sentFrom: mine), "another pane: a late answer moves nothing")
        expect(!EventPaneRules.applies(open: nil, sentFrom: mine), "closed: a late answer moves nothing")

        let body = EventWrite(
            calendar_id: "primary", title: "x", start: "2026-10-08T19:45:00-07:00",
            end: "2026-10-08T21:00:00-07:00", all_day: false, location: nil, description: nil, guests: ["s@x.test"])
        let create = CalendarWrite.createWrite(body, notify: true, createId: "abcde12345", before: [])
        expect(EventPaneRules.refusalText(.refused(.invalid), create, again: false)
               == CalendarWrite.failureText(.invalid, .create, guests: true), "a create's refusal names the guests")
        expect(EventPaneRules.refusalText(.notSent, sent, again: false) == CalendarWrite.failureText(nil, .edit),
               "never sent: says so")
        expect(EventPaneRules.refusalText(.refused(.googleRefused), sent, again: true) == CalendarWrite.resendRefusedText,
               "a refused resend: the first may still have worked")
        expect(EventPaneRules.refusalText(.mayHaveHappened(code: nil), sent, again: false) == nil,
               "may have happened: no refusal toast")

        // The thaw is only on a definite refusal: a first send's 500, bare
        // 502 or undecodable 2xx leaves the pane frozen.
        for e in [APIError(.server, 500, "internal error", code: "internal"), APIError(.server, 502, "bad gateway"),
                  APIError(.unknown, 0, "unexpected response shape"),
                  APIError(.server, 504, "saved, unreadable", code: "uncertain")] {
            let verdict = CalendarWrite.verdict(of: e)
            expect(EventPaneRules.failureEvent(verdict) == nil, "\(e.status) \(e.code ?? "-"): no thaw")
            let state = EventPaneState.sending(again: false).next(.unsure)
            expect(state == .uncertain && !state.fieldsEditable, "settled unseen, it freezes")
        }

        expect(EventPaneRules.composerRefusedText(.editing) == "Save or close the event first", "editable: save or close")
        for state in [EventPaneState.uncertain, .sending(again: true), .sending(again: false)] {
            expect(EventPaneRules.composerRefusedText(state) == "Close the event first",
                   "\(state): a pane that cannot be saved is not offered as one")
        }
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

    /// "Google Meet": a new event's follows its guests until clicked, goes
    /// out only where the calendar makes one, and an edit sends it only when
    /// it moved, never over a Zoom.
    static func aMeetFollowsTheGuestsUntilSet() {
        let cal = Calendar(identifier: .gregorian)
        let today = Date(timeIntervalSince1970: 1_791_000_000)
        var d = EventForm.newDraft(day: today, now: today, calendarId: "primary", calendar: cal)
        d.title = "Planning"
        expect(!EventForm.wantsMeet(d), "alone: no Meet")
        d.guests = "sam@example.com"
        expect(EventForm.wantsMeet(d), "a guest turns it on")
        d.meet = false
        expect(!EventForm.wantsMeet(d), "until the user says otherwise")
        d.meet = true
        let open = GuestTerms(open: true, owner: "me@example.com")
        let wire = { (offered: Bool) in
            (try? EventForm.wire(d, today: today, calendar: cal, guests: open, meet: offered).get())?.meet
        }
        expect(wire(true) == true, "asked for where offered")
        expect(wire(false) == nil, "and never where not")

        func event(_ meeting: String?) -> TimelineEvent {
            var e = TimelineEvent(
                id: "e1", calendar_id: "primary", calendar_name: nil, title: "Sync",
                start: "2026-10-08T10:00:00-07:00", end: "2026-10-08T11:00:00-07:00", all_day: false,
                start_timezone: nil, location: nil, status: .confirmed, self_response: nil, busy: true,
                organizer: nil, html_link: nil, color: nil, editable: true, description: nil)
            e.meeting = meeting
            return e
        }
        let initial = EventForm.draft(editing: event("meet"), today: today, calendar: cal)
        expect(initial.meet == true, "an edit starts from the event's Meet")
        var off = initial
        off.meet = false
        let patch = { (draft: EventDraft, offered: Bool) in
            try? EventForm.patch(draft, from: initial, today: today, calendar: cal, meet: offered).get()
        }
        expect(patch(off, true)?.meet == false, "removed")
        expect(patch(off, true)?.json()["meet"] as? Bool == false, "on the wire")
        expect(patch(initial, true)?.isEmpty == true, "untouched: nothing")
        expect(patch(off, false)?.meet == nil, "not offered: left alone")

        let feed = TimelineFeed(calendars: [
            TimelineCalendar(id: "primary", name: nil, color: nil, primary: true, writable: true, can_invite: true, meet: true),
            TimelineCalendar(id: "team", name: nil, color: nil, primary: false, writable: true, can_invite: false, meet: false),
            TimelineCalendar(id: "old", name: nil, color: nil, primary: false, writable: true, can_invite: nil),
        ])
        expect(CalendarWrite.meetOffered(calendarId: "primary", editing: nil, feed: feed), "a new event")
        expect(!CalendarWrite.meetOffered(calendarId: "team", editing: nil, feed: feed), "not where Google says no")
        expect(!CalendarWrite.meetOffered(calendarId: "old", editing: nil, feed: feed), "nor from an older daemon")
        expect(CalendarWrite.meetOffered(calendarId: "primary", editing: event(nil), feed: feed), "an event with none")
        expect(CalendarWrite.meetOffered(calendarId: "primary", editing: event("meet"), feed: feed), "its own Meet")
        expect(!CalendarWrite.meetOffered(calendarId: "primary", editing: event("zoom"), feed: feed), "never over a Zoom")
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
