// The title that understands "dinner with steven on friday 4pm"
// (Lib/TitleParse.swift), and the guest it offers (Lib/EventGuests.swift).
//
// Each of these is wrong only in somebody's calendar: a "May day parade" filed
// on May 1st of next year, "meet at 4" moved to 4pm, a "call with 4pm people"
// renamed "call with people", a day the user picked by hand overwritten by a
// word typed later in the title, words deleted from the title that leave the
// event on the Friday they named, a stranger offered as a guest, a suggestion
// that counts as a guest before its checkmark. Today is the event suite's:
// Monday October 5 2026, 3:40pm in San Francisco.

import Foundation

@main
struct TitleParseTests {
    nonisolated(unsafe) static var failures = 0
    nonisolated(unsafe) static var checks = 0

    static let zone = TimeZone(identifier: "America/Los_Angeles")!
    static var cal: Calendar { calendar(firstWeekday: 1) }
    static func calendar(firstWeekday: Int) -> Calendar {
        var c = Calendar(identifier: .gregorian)
        c.timeZone = zone
        c.firstWeekday = firstWeekday
        return c
    }
    static let now = at("2026-10-05T15:40:00-07:00")

    static func main() {
        thePhrase()
        days()
        times()
        ranges()
        negatives()
        twoOfAKindIsNeither()
        names()
        theSavedTitle()
        aFieldSetByHandKeepsItsWords()
        theTitleFillsTheWells()
        deletingTheWordsPutsThingsBack()
        aHandSetWellIsNeverOverwritten()
        escTakesTheTitleAsTyped()
        anEditIsNeverRead()
        allDayTakesNoTime()
        theSaveUsesTheSavedTitle()
        theEndDayFollowsUntilSaid()
        whoIsOffered()
        aSuggestionIsNotAGuest()
        answeringASuggestion()
        whatTheDaemonIsAsked()
        theWire()

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

    static func at(_ s: String) -> Date { AgendaTime.parse(s, zone: nil, calendar: cal)!.date }
    static func day(_ s: String) -> Date { cal.startOfDay(for: at(s + "T12:00:00-07:00")) }

    static func read(_ s: String, _ c: Calendar = cal, today: Date = now) -> TitleReading {
        TitleParse.read(s, today: today, calendar: c)
    }

    /// A reading in one line: "title | Oct 9 | 960-1020 | names".
    static func line(_ s: String) -> String {
        let r = read(s)
        let d = r.day.map { Agenda.format($0, "MMM d", cal) } ?? "-"
        let t = r.start.map { "\($0)" + (r.end.map { "-\($0)" } ?? "") } ?? "-"
        return "\(r.title) | \(d) | \(t) | \(r.names.joined(separator: ","))"
    }

    /// A new event's draft, as `n` opens it on today: 4pm to 5pm.
    static func fresh() -> EventDraft {
        EventForm.newDraft(day: now, now: now, calendarId: "primary", calendar: cal)
    }

    static func typed(_ d: EventDraft, _ title: String) -> EventDraft {
        EventForm.retitled(d, title: title, today: now, calendar: cal)
    }

    /// The title typed a letter at a time, as it is on screen.
    static func typedByLetter(_ d: EventDraft, _ title: String) -> EventDraft {
        var out = d
        var sofar = ""
        for ch in title {
            sofar.append(ch)
            out = typed(out, sofar)
        }
        return out
    }

    // MARK: reading

    /// The founder's own example.
    static func thePhrase() {
        let r = read("dinner with steven on friday 4pm")
        expect(r.title == "Dinner with steven", "the day and time come out, \"with steven\" stays, one capital: \(r.title)")
        expect(r.day == day("2026-10-09"), "friday is the coming Friday, Oct 9")
        expect(r.start == 16 * 60 && r.end == nil, "4pm, and no end said")
        expect(r.names == ["steven"], "with steven")
        expect(r.filled, "words came out of the title")
    }

    static func days() {
        let table: [(String, String)] = [
            ("dinner on friday", "Dinner | Oct 9 | - | "),
            ("dinner friday", "Dinner | Oct 9 | - | "),
            ("Friday dinner", "dinner | Oct 9 | - | "),
            ("lunch today", "Lunch | Oct 5 | - | "),
            ("drinks tonight", "Drinks | Oct 5 | - | "),
            ("lunch tomorrow", "Lunch | Oct 6 | - | "),
            ("offsite oct 12", "Offsite | Oct 12 | - | "),
            ("offsite on october 12th", "Offsite | Oct 12 | - | "),
            ("offsite 12 oct", "Offsite | Oct 12 | - | "),
            ("offsite on 10/12", "Offsite | Oct 12 | - | "),
            ("review next tue", "Review | Oct 13 | - | "),
            ("review this thu", "Review | Oct 8 | - | "),
            ("review on mon", "Review | Oct 5 | - | "),
            ("review fri oct 16", "Review | Oct 16 | - | "),
            ("gym sat", "Gym | Oct 10 | - | "),
            ("dentist, friday", "Dentist | Oct 9 | - | "),
            ("planning sept 3 2027", "Planning | Sep 3 | - | "),
        ]
        for (typed, want) in table {
            expect(line(typed) == want, "\"\(typed)\" reads \(line(typed)), wanted \(want)")
        }
        expect(read("planning sept 3 2027").day == day("2027-09-03"), "a year, when one is said")
        // "next tue" is next WEEK's, by the user's own weeks. On Sunday Oct 4:
        // Sunday-first weeks put it on the 13th, Monday-first on the 6th.
        let sunday = at("2026-10-04T10:00:00-07:00")
        expect(read("review next tue", calendar(firstWeekday: 1), today: sunday).day == day("2026-10-13"),
               "next tue from a Sunday, weeks from Sunday: the 13th")
        expect(read("review next tue", calendar(firstWeekday: 2), today: sunday).day == day("2026-10-06"),
               "weeks from Monday: the 6th")
        expect(read("review next mon").day == day("2026-10-12"), "next mon is never today")
    }

    static func times() {
        let table: [(String, String)] = [
            ("standup 9am", "Standup | - | 540 | "),
            ("standup at 9am", "Standup | - | 540 | "),
            ("standup 9 am", "Standup | - | 540 | "),
            ("standup 9:15am", "Standup | - | 555 | "),
            ("standup 9.15am", "Standup | - | 555 | "),
            ("call at 4:30", "Call | - | 990 | "),
            ("call @ 16:30", "Call | - | 990 | "),
            ("call 16:30", "Call | - | 990 | "),
            ("lunch at noon", "Lunch | - | 720 | "),
            ("lunch friday noon", "Lunch | Oct 9 | 720 | "),
            ("4pm standup", "Standup | - | 960 | "),
            ("call 4 p.m.", "Call | - | 960 | "),
            ("dinner fri 7pm", "Dinner | Oct 9 | 1140 | "),
            ("dinner 7pm fri", "Dinner | Oct 9 | 1140 | "),
        ]
        for (typed, want) in table {
            expect(line(typed) == want, "\"\(typed)\" reads \(line(typed)), wanted \(want)")
        }
    }

    static func ranges() {
        let table: [(String, String)] = [
            ("workshop 4-6pm", "Workshop | - | 960-1080 | "),
            ("workshop 4pm-6pm", "Workshop | - | 960-1080 | "),
            ("workshop 11-1pm", "Workshop | - | 660-780 | "),
            ("workshop 9-11am", "Workshop | - | 540-660 | "),
            ("workshop from 4 to 5:30", "Workshop | - | 960-1050 | "),
            ("workshop 4 to 6pm", "Workshop | - | 960-1080 | "),
            ("workshop 4pm to 6", "Workshop | - | 960-1080 | "),
            ("workshop 4pm until 6pm", "Workshop | - | 960-1080 | "),
            ("party 10pm-2am", "Party | - | 1320-120 | "),
            ("party 10-2am", "Party | - | 1320-120 | "),
            ("workshop friday from 4 to 5:30", "Workshop | Oct 9 | 960-1050 | "),
        ]
        for (typed, want) in table {
            expect(line(typed) == want, "\"\(typed)\" reads \(line(typed)), wanted \(want)")
        }
    }

    /// Titles that say nothing about when: returned EXACTLY as typed, no
    /// capital added, nothing filled.
    static func negatives() {
        let titles = [
            "May day parade", "call with 4pm people", "lunch at Friday's", "meet at 4",
            "meet at 4 ", "Q4 planning", "5k run", "top 10 at 10", "room 4a", "catch 22",
            "version 4.30 review", "4 to 6 people", "wedding plans", "sunny side up brunch",
            "sun protection talk", "sat down with the team again", "mon ami dinner plans",
            "the march of progress", "midnight mass rehearsal", "high noon screening",
            "from 4", "at", "on", "next steps", "this is it", "1930s film night",
            "call with friday crew", "dinner with may", "iphone 15 pm launch", "on 10/12/",
            "ship v2 by eod", "10/12 retro", "follow-up", "9-5 mindset",
        ]
        for title in titles {
            let r = read(title)
            expect(r.day == nil && r.start == nil && r.end == nil, "\"\(title)\" names no day and no time")
            expect(r.title == title.trimmingCharacters(in: .whitespaces), "\"\(title)\" is saved as typed: \(r.title)")
            expect(!r.filled, "\"\(title)\" fills nothing")
        }
        expect(read("call with 4pm people").names.isEmpty, "and \"4pm\" is nobody's name")
        expect(read("").title == "" && read("   ").title == "", "an empty title is an empty title")
    }

    /// Ambiguity resolves to doing nothing, each kind on its own.
    static func twoOfAKindIsNeither() {
        let twoDays = read("friday planning for monday 4pm")
        expect(twoDays.day == nil, "two different days: neither")
        expect(twoDays.start == 960, "the one time is still the time")
        expect(twoDays.title == "Friday planning for monday", "and only its words come out: \(twoDays.title)")
        let twoTimes = read("move 3pm sync to 4pm friday")
        expect(twoTimes.start == nil && twoTimes.end == nil, "two different times: neither")
        expect(twoTimes.day == day("2026-10-09"), "the one day is still the day")
        expect(read("friday dinner on friday").day == day("2026-10-09"), "the same day said twice is that day")
        expect(read("friday dinner on friday").title == "Dinner", "and both come out")
        let both = read("3pm monday or 4pm tuesday")
        expect(!both.filled && both.title == "3pm monday or 4pm tuesday", "two of each: the title is left alone")
    }

    static func names() {
        expect(read("dinner with steven and ana").names == ["steven", "ana"], "two names")
        expect(read("dinner with Steven, Ana and Bo").names == ["Steven", "Ana", "Bo"], "three, as typed")
        expect(read("dinner with steven & ana friday").names == ["steven", "ana"], "an ampersand, and the day ends them")
        expect(read("dinner with steven ng tomorrow").names == ["steven ng"], "a name of two words")
        expect(read("dinner with steven ng lee park").names == ["steven ng"], "and no more than two")
        expect(read("coffee with april").names == ["april"], "April is a person")
        expect(read("coffee with april").day == nil, "and not a month")
        expect(read("coffee with ana oct 12").names == ["ana"], "a date ends the names")
        expect(read("coffee with ana oct 12").day == day("2026-10-12"), "and is still the date")
        expect(read("coffee with ana 4pm").names == ["ana"] && read("coffee with ana 4pm").start == 960,
               "a time ends the names")
        expect(read("sync with the team").names.isEmpty, "\"the team\" is not a name")
        expect(read("lunch with").names.isEmpty, "\"with\" alone names nobody")
        expect(read("dinner with a, b, c, d, ef, gh").names.count <= TitleParse.maxNames, "a cap on names")
        expect(read("dinner with al and jo and sam and kim and lee").names.count == TitleParse.maxNames,
               "the cap is \(TitleParse.maxNames)")
        expect(read("with steven").names == ["steven"], "a title that is only \"with steven\"")
        expect(read("dinner with steven").title == "dinner with steven", "a name alone changes nothing in the title")
        expect(!read("dinner with steven").filled, "and fills nothing")
        expect(read("Dinner w/ steven").names.isEmpty, "\"w/\" is not read")
    }

    static func theSavedTitle() {
        expect(read("Dinner with Steven on Friday 4pm").title == "Dinner with Steven", "capitals typed are kept")
        expect(read("dinner with Steven on friday").title == "dinner with Steven",
               "one capital typed anywhere: the first letter is left as typed")
        expect(read("friday 4pm").title == "", "nothing but a day and a time leaves no title")
        expect(read("on friday at 4pm dinner").title == "Dinner", "leading words go with what they led")
        expect(read("dinner - friday 7pm").title == "Dinner", "a dash left hanging is dropped")
        expect(read("dinner: friday").title == "Dinner", "and a colon")
        expect(read("dinner  with   steven   friday").title == "Dinner with steven", "spaces are single")
        expect(read("  dinner friday  ").title == "Dinner", "and trimmed")
        expect(read("école friday").title == "École", "the capital is the letter's own")
        expect(!read("dinner with steven on friday 4pm").title.contains("\u{2014}"), "no em dash")
    }

    /// Words for a field the user set by hand stay in the title: nothing is
    /// taken out that did not fill something.
    static func aFieldSetByHandKeepsItsWords() {
        let noDay = TitleParse.read("dinner friday 4pm", today: now, calendar: cal, takeDay: false)
        expect(noDay.day == nil && noDay.start == 960, "the day is not taken, the time is")
        expect(noDay.title == "Dinner friday", "and \"friday\" stays in the title: \(noDay.title)")
        let noTime = TitleParse.read("dinner friday 4pm", today: now, calendar: cal, takeTime: false)
        expect(noTime.day == day("2026-10-09") && noTime.start == nil, "the time is not taken, the day is")
        expect(noTime.title == "Dinner 4pm", "and \"4pm\" stays: \(noTime.title)")
        let neither = TitleParse.read("dinner friday 4pm", today: now, calendar: cal, takeDay: false, takeTime: false)
        expect(!neither.filled && neither.title == "dinner friday 4pm", "neither taken: exactly as typed")
    }

    // MARK: the title in the form

    static func theTitleFillsTheWells() {
        let base = fresh()
        expect(base.titleRead.on && base.date == "Mon Oct 5" && base.start == "4pm" && base.end == "5pm",
               "a new event starts read, today at the next hour")
        let d = typedByLetter(base, "dinner with steven on friday 7pm")
        expect(d.title == "dinner with steven on friday 7pm", "the field keeps the words as typed")
        expect(d.date == "Fri Oct 9", "the day well is filled: \(d.date)")
        expect(d.start == "7pm" && d.end == "8pm", "the times too, an hour long: \(d.start) to \(d.end)")
        expect(Set(d.titleRead.filled.keys) == [.date, .start, .end], "all three are marked as the title's")
        expect(d.titleRead.filled[.date] == "Mon Oct 5" && d.titleRead.filled[.start] == "4pm",
               "and what was there before is kept for the way back")
        expect(EventForm.savedTitle(d, today: now, calendar: cal) == "Dinner with steven", "the saved title")
        let ranged = typed(base, "workshop friday 2-4pm")
        expect(ranged.start == "2pm" && ranged.end == "4pm", "a range fills both ends")
        let overnight = typed(base, "party sat 10pm-2am")
        expect(overnight.start == "10pm" && overnight.end == "2am", "an overnight range as it is said")
        if case .success(let w) = EventForm.when(overnight, today: now, calendar: cal) {
            expect(EventForm.summary(w, calendar: cal) == "Sat Oct 10, 10pm to 2am Sun", "and it ends the next day")
        } else {
            expect(false, "the overnight draft resolves")
        }
        // Idempotent: the same title again changes nothing.
        expect(typed(d, d.title) == d, "typing nothing new changes nothing")
    }

    static func deletingTheWordsPutsThingsBack() {
        let base = fresh()
        let filled = typed(base, "dinner friday 7pm")
        let noTime = typed(filled, "dinner friday")
        expect(noTime.date == "Fri Oct 9", "the day is still named")
        expect(noTime.start == "4pm" && noTime.end == "5pm", "the times are back to what they were")
        expect(noTime.titleRead.filled.keys.map(\.rawValue).sorted() == ["date"], "and no longer the title's")
        let plain = typed(noTime, "dinner")
        expect(plain.date == "Mon Oct 5", "the day is back too")
        expect(plain.titleRead.filled.isEmpty, "nothing is the title's")
        expect(plain == typed(base, "dinner"), "exactly the draft that never had the words")
        // Changing the word re-fills from the ORIGINAL, never from the fill.
        let moved = typed(typed(base, "dinner friday"), "dinner saturday")
        expect(moved.date == "Sat Oct 10" && moved.titleRead.filled[.date] == "Mon Oct 5",
               "a changed word re-fills, and the way back is still to the start")
        expect(typed(moved, "").date == "Mon Oct 5", "an emptied title un-fills everything")
    }

    static func aHandSetWellIsNeverOverwritten() {
        var d = fresh()
        // The user types a day into the well themselves.
        d.date = "Oct 20"
        d = EventForm.handSet(d, .date)
        d = typedByLetter(d, "dinner friday 7pm")
        expect(d.date == "Oct 20", "a day set by hand survives a day typed in the title")
        expect(d.start == "7pm", "the time, not set by hand, is still filled")
        expect(EventForm.savedTitle(d, today: now, calendar: cal) == "Dinner friday",
               "and \"friday\" stays in the title, since it filled nothing")
        // Filled by the title first, THEN set by hand: the hand wins from there.
        var e = typed(fresh(), "dinner friday 7pm")
        e.start = "8pm"
        e = EventForm.handSet(e, .start)
        e = typed(e, "dinner friday 6pm")
        expect(e.start == "8pm", "a time typed over the title's stays")
        expect(e.titleRead.filled[.start] == nil, "and is not the title's any more")
        expect(e.end == "8pm", "its end is not touched either, start and end go together: \(e.end)")
        expect(e.date == "Fri Oct 9", "the day is still the title's")
        e = typed(e, "dinner")
        expect(e.start == "8pm", "deleting the words does not un-set a hand-set time")
        expect(e.date == "Mon Oct 5", "and puts the day back")
        // A day clicked on the grid beside the pane is set by hand.
        let clicked = EventForm.moved(typed(fresh(), "dinner friday"), to: day("2026-10-21"), today: now, calendar: cal)
        expect(clicked.date == "Wed Oct 21" && clicked.titleRead.hand.contains(.date), "a clicked day is the user's")
        expect(typed(clicked, "dinner saturday").date == "Wed Oct 21", "and later typing leaves it alone")
        // A swept draft: the day and the times are the user's from the start.
        let swept = EventForm.newDraft(
            day: day("2026-10-07"), startMinutes: 600, endMinutes: 660, now: now, calendarId: "primary", calendar: cal)
        let named = typed(swept, "sync with ana friday 3pm")
        expect(named.date == "Wed Oct 7" && named.start == "10am", "a swept span is never moved by its title")
        expect(EventForm.titleReading(named, today: now, calendar: cal)?.names == ["ana"], "but who it is with is read")
        expect(EventForm.savedTitle(named, today: now, calendar: cal) == "sync with ana friday 3pm",
               "and its title is saved as typed")
    }

    static func escTakesTheTitleAsTyped() {
        let d = typed(fresh(), "dinner friday 7pm")
        expect(EventForm.titleIsRead(d, today: now, calendar: cal), "there is something for Esc to undo")
        let literal = EventForm.literalTitle(d, today: now, calendar: cal)
        expect(literal.title == "dinner friday 7pm", "the typed words stay in the field")
        expect(literal.date == "Mon Oct 5" && literal.start == "4pm" && literal.end == "5pm",
               "and everything they filled goes back")
        expect(EventForm.savedTitle(literal, today: now, calendar: cal) == "dinner friday 7pm",
               "the title is saved as typed")
        expect(!EventForm.titleIsRead(literal, today: now, calendar: cal), "a second Esc has nothing to undo here")
        expect(typed(literal, "dinner friday 7pm sharp").date == "Mon Oct 5", "more typing is still literal")
        let again = typed(typed(literal, ""), "lunch friday")
        expect(again.date == "Fri Oct 9", "an emptied title is read afresh")
        expect(!EventForm.titleIsRead(typed(fresh(), "dinner with steven"), today: now, calendar: cal),
               "a title that filled nothing leaves Esc to close the pane")
    }

    static func anEditIsNeverRead() {
        var d = EventDraft()
        (d.title, d.calendarId, d.date, d.start, d.end) = ("Dinner on Friday 4pm", "primary", "Thu Oct 8", "7pm", "8pm")
        expect(!d.titleRead.on, "a draft that was not made new is not read")
        let renamed = typed(d, "Dinner on Saturday 5pm")
        expect(renamed.date == "Thu Oct 8" && renamed.start == "7pm", "an edit's title moves nothing")
        expect(EventForm.savedTitle(renamed, today: now, calendar: cal) == "Dinner on Saturday 5pm",
               "and is saved as typed")
        expect(EventForm.titleReading(renamed, today: now, calendar: cal) == nil, "no reading, so no suggestion")
    }

    static func allDayTakesNoTime() {
        var d = fresh()
        d.allDay = true
        d = typed(d, "offsite friday 4pm")
        expect(d.date == "Fri Oct 9", "an all-day event takes the day")
        expect(d.start == "4pm" && d.titleRead.filled[.start] == nil, "and no time: the wells it hides are left")
        expect(EventForm.savedTitle(d, today: now, calendar: cal) == "Offsite 4pm", "so \"4pm\" stays in the title")
    }

    static func theSaveUsesTheSavedTitle() {
        let d = typed(fresh(), "dinner with steven on friday 7pm")
        guard case .success(let body) = EventForm.wire(d, today: now, calendar: cal) else {
            expect(false, "the read draft saves")
            return
        }
        expect(body.title == "Dinner with steven", "the body carries the saved title: \(body.title)")
        expect(body.start == "2026-10-09T19:00:00-07:00" && body.end == "2026-10-09T20:00:00-07:00",
               "on the day and at the time it named")
        expect(body.guests == nil, "and names nobody: a name in the title is not a guest")
        let chip = EventForm.provisional(d, color: nil, today: now, calendar: cal)
        expect(chip?.title == "Dinner with steven", "the grid's chip shows the saved title")
        let empty = typed(fresh(), "friday 7pm")
        expect(EventForm.problem(empty, today: now, calendar: cal) == .noTitle,
               "a title that was only a day and a time needs a title")
        let long = typed(fresh(), String(repeating: "a", count: 200) + " friday")
        expect(EventForm.problem(long, today: now, calendar: cal) == nil, "the 200 are counted after the words come out")
    }

    // MARK: the end day

    static func theEndDayFollowsUntilSaid() {
        var d = fresh()
        (d.date, d.start, d.end) = ("Thu Oct 8", "7pm", "8pm")
        expect(d.endDate == "", "a new event's end day is not said")
        expect(EventForm.impliedEndDate(d, today: now, calendar: cal) == "Thu Oct 8", "it follows the start's day")
        d.date = "Fri Oct 9"
        expect(EventForm.impliedEndDate(d, today: now, calendar: cal) == "Fri Oct 9", "and moves with it")
        d.end = "2am"
        expect(EventForm.impliedEndDate(d, today: now, calendar: cal) == "Sat Oct 10",
               "an end time before the start rolls to the next day")
        d.end = "7pm"
        expect(EventForm.impliedEndDate(d, today: now, calendar: cal) == "Sat Oct 10",
               "and so does one AT the start")
        func summary(_ d: EventDraft) -> String {
            switch EventForm.when(d, today: now, calendar: cal) {
            case .success(let w): return EventForm.summary(w, calendar: cal)
            case .failure(let p): return "problem: \(p)"
            }
        }
        expect(summary(d) == "Fri Oct 9, 7pm to 7pm Sat", "the line under the fields says the whole day: \(summary(d))")
        d.end = "9pm"
        // The user types an end day: it stops following.
        d.endDate = "Oct 11"
        expect(summary(d) == "Fri Oct 9, 7pm to 9pm Sun Oct 11", "a said end day is the end day: \(summary(d))")
        d.date = "Sat Oct 10"
        expect(summary(d) == "Sat Oct 10, 7pm to 9pm Sun", "and stays put when the start moves: \(summary(d))")
        d.date = "Mon Oct 12"
        expect(EventForm.when(d, today: now, calendar: cal) == .failure(.endsBeforeStart),
               "a start moved past it is said, not silently rolled")
        d.endDate = ""
        expect(summary(d) == "Mon Oct 12, 7pm to 9pm", "emptied, it follows again")
        // The wire carries it.
        d.title = "Retreat"
        d.endDate = "Oct 14"
        if case .success(let body) = EventForm.wire(d, today: now, calendar: cal) {
            expect(body.start == "2026-10-12T19:00:00-07:00" && body.end == "2026-10-14T21:00:00-07:00",
                   "a timed event over three days, as typed")
        } else {
            expect(false, "a multi-day timed draft saves")
        }
        // A same-day end day with an end time before the start is an error,
        // because the day was said.
        d.endDate = "Oct 12"
        d.end = "6pm"
        expect(EventForm.when(d, today: now, calendar: cal) == .failure(.endsBeforeStart),
               "an end day said by hand is taken at its word")
        // All day has no implied end day; unreadable starts have none.
        var a = d
        a.allDay = true
        expect(EventForm.impliedEndDate(a, today: now, calendar: cal) == nil, "all day has its own last-day well")
        d.date = "someday"
        expect(EventForm.impliedEndDate(d, today: now, calendar: cal) == nil, "no start day, nothing to follow")
        // A grid click keeps a said run's length and leaves a following one following.
        var run = fresh()
        (run.date, run.start, run.end, run.endDate) = ("Oct 8", "7pm", "9pm", "Oct 10")
        let moved = EventForm.moved(run, to: day("2026-10-20"), today: now, calendar: cal)
        expect(moved.date == "Tue Oct 20" && moved.endDate == "Thu Oct 22", "a clicked day carries a said end day with it")
        run.endDate = ""
        expect(EventForm.moved(run, to: day("2026-10-20"), today: now, calendar: cal).endDate == "",
               "and a following one keeps following")
    }

    // MARK: who is offered

    static let steven = NamedPerson(addr: "steven@example.com", name: "Steven Ng")
    static let stevenP = NamedPerson(addr: "s.park@example.org", name: "Steven Park")
    static let ana = NamedPerson(addr: "ana@example.com", name: nil)

    static func offer(
        _ d: EventDraft, people: [String: [NamedPerson]], access: GuestAccess = .field, owner: String? = "me@example.com"
    ) -> GuestSuggestion? {
        EventGuests.suggestion(
            d, reading: EventForm.titleReading(d, today: now, calendar: cal), access: access, people: people,
            owner: owner)
    }

    static func whoIsOffered() {
        let d = typed(fresh(), "dinner with steven and ana friday")
        let people = ["steven": [steven, stevenP], "ana": [ana]]
        let s = offer(d, people: people)
        expect(s?.name == "steven" && s?.best == steven, "the first name's best candidate")
        expect(s?.others == [stevenP], "the others, quietly")
        expect(offer(d, people: [:]) == nil, "nothing until the daemon has answered")
        expect(offer(d, people: ["steven": [], "ana": [ana]])?.best == ana,
               "a name nobody answers to is passed over for the next")
        expect(offer(d, people: people, access: .none) == nil, "a daemon without guests offers nobody")
        expect(offer(d, people: people, access: .closed(.otherCalendar)) == nil,
               "a calendar that may not invite offers nobody")
        // Never the user themselves, never a non-address, never more than three.
        let me = NamedPerson(addr: "ME@example.com", name: "Steven Me")
        expect(offer(d, people: ["steven": [me, steven]])?.best == steven, "the account's own address is never offered")
        expect(offer(d, people: ["steven": [me]])?.name != "steven", "and a name that is only the user is nobody")
        expect(offer(d, people: ["steven": [NamedPerson(addr: "not an address", name: "Steven"), steven]])?.best == steven,
               "something that is not an address is never offered")
        let many = (1...6).map { NamedPerson(addr: "s\($0)@example.com", name: "Steven \($0)") }
        let capped = offer(d, people: ["steven": many])
        expect((capped?.others.count ?? 0) + 1 == EventGuests.suggestionCap, "three candidates at most")
        expect(offer(d, people: ["steven": [steven, NamedPerson(addr: "STEVEN@example.com", name: "S")]])?.others.isEmpty == true,
               "the same address twice is one person")
        // Typed by hand already: nothing to suggest for that name.
        var invited = d
        invited.guests = "Steven@Example.com,"
        expect(offer(invited, people: people)?.name == "ana", "someone already on the line is not offered again")
        var other = d
        other.guests = "s.park@example.org,"
        expect(offer(other, people: people)?.name == "ana", "nor is the name, once any of its candidates is on it")
        // An edit, and a title taken literally, are not read: nobody offered.
        var edit = EventDraft()
        (edit.title, edit.calendarId, edit.date, edit.start, edit.end) = ("dinner with steven", "primary", "Oct 8", "7pm", "8pm")
        expect(offer(edit, people: people) == nil, "an edit's title offers nobody")
        expect(offer(EventForm.literalTitle(d, today: now, calendar: cal), people: people) == nil,
               "a title taken as typed offers nobody")
        expect(EventGuests.namesToAsk(d, reading: EventForm.titleReading(d, today: now, calendar: cal), access: .field)
            == ["steven", "ana"], "the daemon is asked about each name once")
        expect(EventGuests.namesToAsk(d, reading: EventForm.titleReading(d, today: now, calendar: cal), access: .none).isEmpty,
               "and about nobody where nobody can be invited")
    }

    /// The suggestion never changes what a save does: the controls, the body
    /// and the Enter rule are those of a draft with no guests.
    static func aSuggestionIsNotAGuest() {
        let d = typed(fresh(), "dinner with steven friday 7pm")
        let people = ["steven": [steven]]
        expect(offer(d, people: people) != nil, "a suggestion is showing")
        expect(EventGuests.read(d.guests).addresses.isEmpty, "and the guests line is empty")
        let controls = EventGuests.controls(editing: false, before: 0, after: EventGuests.read(d.guests).addresses.count)
        expect(controls == EventGuests.controls(editing: false, before: 0, after: 0), "the save control is the plain one")
        expect(EventGuests.enterMaySave(controls), "plain Enter still saves")
        let open = GuestTerms(open: true, owner: "me@example.com")
        if case .success(let body) = EventForm.wire(d, today: now, calendar: cal, guests: open) {
            expect(body.guests == nil && body.notify == nil, "the body invites nobody and tells nobody")
        } else {
            expect(false, "the draft saves")
        }
    }

    static func answeringASuggestion() {
        let d = typed(fresh(), "dinner with steven and ana friday")
        let people = ["steven": [steven, stevenP], "ana": [ana]]
        // The checkmark.
        let added = EventGuests.accepting(steven, for: "steven", in: d)
        expect(EventGuests.read(added.guests).addresses == ["steven@example.com"], "the checkmark makes a real guest")
        expect(EventGuests.read(added.guests).unread.isEmpty && added.guests.hasSuffix(","), "as a whole pill")
        expect(offer(added, people: people)?.name == "ana", "and the next name is offered")
        expect(EventGuests.controls(editing: false, before: 0, after: 1).primary.label.contains("invite"),
               "now the control says it will invite")
        // An "or" candidate instead of the best.
        let alt = EventGuests.accepting(stevenP, for: "steven", in: d)
        expect(EventGuests.read(alt.guests).addresses == ["s.park@example.org"], "the other Steven, when picked")
        expect(offer(alt, people: people)?.name == "ana", "answers the name just the same")
        // Kept beside guests typed already, and never twice.
        var some = d
        some.guests = "bo@example.com"
        let both = EventGuests.accepting(steven, for: "steven", in: some)
        expect(EventGuests.read(both.guests).addresses == ["bo@example.com", "steven@example.com"], "after who is there")
        expect(EventGuests.split(EventGuests.accepting(steven, for: "steven", in: both).guests).count == 2,
               "accepting twice adds once: the line itself holds two")
        // Taken off the line again by hand: the name was answered, and stays so.
        var removed = added
        removed.guests = ""
        expect(offer(removed, people: people)?.name == "ana",
               "a guest accepted and then removed is not offered a second time")
        // The x.
        let no = EventGuests.dismissing("steven", in: d)
        expect(EventGuests.read(no.guests).addresses.isEmpty, "the x adds nobody")
        expect(offer(no, people: people)?.name == "ana", "and moves on")
        let retyped = typed(typed(no, "dinner with ana friday"), "dinner with Steven and ana friday")
        expect(offer(retyped, people: people)?.name == "ana", "a dismissed name does not come back, however it is retyped")
        let done = EventGuests.dismissing("ana", in: no)
        expect(offer(done, people: people) == nil, "both answered: nothing on offer")
        expect(EventGuests.namesToAsk(done, reading: EventForm.titleReading(done, today: now, calendar: cal), access: .field).isEmpty,
               "and nothing more to ask")
        expect(done != d, "an answered suggestion is a touched draft, so the pane parks rather than closes")
    }

    static func whatTheDaemonIsAsked() {
        expect(EventGuests.queries(for: "Steven") == ["steven"], "one word, folded")
        expect(EventGuests.queries(for: "ana  Downtown") == ["ana downtown", "ana"], "two words, then the first alone")
        expect(EventGuests.queries(for: "  ").isEmpty, "nothing for nothing")
        expect(EventGuests.nameKey(" Steven  NG ") == "steven ng", "a name's key is folded and single-spaced")
    }

    static func theWire() {
        func decode(_ json: String) -> NamedPeople? {
            try? JSONDecoder().decode(NamedPeople.self, from: Data(json.utf8))
        }
        let full = decode(#"{"people":[{"addr":"steven@example.com","name":"Steven Ng"},{"addr":"s@x.org","name":null}]}"#)
        expect(full?.people == [steven, NamedPerson(addr: "s@x.org", name: nil)], "names and a missing name")
        expect(decode(#"{"people":[{"addr":"a@b.co"}]}"#)?.people.first?.name == nil, "no name key at all")
        expect(decode("{}")?.people.isEmpty == true, "no list is nobody")
        expect(decode(#"{"people":"nope"}"#)?.people.isEmpty == true, "a list that is not one is nobody")
    }
}
