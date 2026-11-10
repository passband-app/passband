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
        rule1OnlyTheEndIsRead()
        rule2CapitalsDoNotMatter()
        rule3OnlyALedRunComesOut()
        rule4AllOrNothing()
        rule5SomeWordsLeadNothing()
        rule6ShortWordsNeedCompany()
        rule7OnlyAppliedWordsComeOut()
        rule7AHandSetWellWins()
        rule8TheSavesAsLine()
        theProbes()
        ranges()
        days()
        names()
        theSavedTitle()
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
        aFailedLookupIsNotAnAnswer()
        theKeyboardAndTheSuggestion()
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
        // The year only when it is not this one.
        let d = r.day.map { Agenda.format($0, cal.component(.year, from: $0) == 2026 ? "MMM d" : "MMM d yyyy", cal) } ?? "-"
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

    static func table(_ rows: [(String, String)]) {
        for (typed, want) in rows {
            expect(line(typed) == want, "\"\(typed)\" reads \(line(typed)), wanted \(want)")
        }
    }

    /// Nothing read: no day, no time, and the title exactly as typed.
    static func untouched(_ titles: [String], _ why: String) {
        for title in titles {
            let r = read(title)
            expect(r.day == nil && r.start == nil && r.end == nil, "\(why): \"\(title)\" names no day and no time")
            expect(r.title == title.trimmingCharacters(in: .whitespaces), "\(why): \"\(title)\" is saved as typed: \(r.title)")
            expect(!r.filled, "\(why): \"\(title)\" fills nothing")
        }
    }

    /// The canonical example.
    static func thePhrase() {
        let r = read("dinner with steven on friday 4pm")
        expect(r.title == "Dinner with Steven", "the day and time come out, \"with steven\" stays, one capital: \(r.title)")
        expect(r.day == day("2026-10-09"), "friday is the coming Friday, Oct 9")
        expect(r.start == 16 * 60 && r.end == nil, "4pm, and no end said")
        expect(r.names == ["steven"], "with steven")
        expect(r.filled, "the wells are filled")
        // And who, for the suggestion: the pane asks about "steven".
        let d = typed(fresh(), "dinner with steven on friday 4pm")
        expect(EventGuests.namesToAsk(d, reading: EventForm.titleReading(d, today: now, calendar: cal), access: .field) == ["steven"],
               "Steven is the name looked up")
    }

    /// RULE 1: only the end of the title is read, after a closing "with ..."
    /// is set aside.
    static func rule1OnlyTheEndIsRead() {
        table([
            // Both orders of the same words.
            ("dinner friday 4pm with steven", "dinner friday 4pm with steven | Oct 9 | 960 | steven"),
            ("dinner with steven friday 4pm", "dinner with steven friday 4pm | Oct 9 | 960 | steven"),
            ("dinner on friday 4pm with steven", "Dinner with Steven | Oct 9 | 960 | steven"),
            ("dinner on friday with steven and ana", "Dinner with Steven and Ana | Oct 9 | - | steven,ana"),
            // A closing "with" that names nobody is still set aside.
            ("lunch friday with the team", "lunch friday with the team | Oct 9 | - | "),
            ("lunch friday with mom and dad", "lunch friday with mom and dad | Oct 9 | - | mom,dad"),
            // The run is the END: as far back as the words are day and time
            // words, and no further.
            ("lunch friday noon", "lunch friday noon | Oct 9 | 720 | "),
            ("dinner 7pm fri", "dinner 7pm fri | Oct 9 | 1140 | "),
            ("a b c d e f g h i j friday", "a b c d e f g h i j friday | Oct 9 | - | "),
            ("dinner - friday", "dinner - friday | Oct 9 | - | "),
        ])
        untouched([
            // The brief's five.
            "Black Friday sale", "Never on Sunday screening", "Meet 2 PM candidates", "Vote on 1/2 measure",
            "USA Today interview",
            // Led, and still not at the end: nothing.
            "on friday dinner with steven", "at 4pm standup", "standup at 9:15 daily", "this fri retro notes",
            "Next Friday dinner", "notes from 3 to 4pm people", "4pm standup", "friday dinner plans",
            "Friday dinner", "tomorrow people screening", "sun oct 11 hike", "flight 4pm lax",
            // The word after "with" is never a day or a time.
            "call with friday", "call with 4pm people", "call with friday crew", "meet with april 5",
            // A closing "with" clause that itself says when is not set aside,
            // so the title's last word decides.
            "dinner friday with steven at nobu",
        ], "rule 1")
        expect(read("dinner with may").names == ["may"] && !read("dinner with may").filled, "May is a person after \"with\"")
    }

    /// RULE 2: capitals do not matter inside the run.
    static func rule2CapitalsDoNotMatter() {
        table([
            ("Dinner Friday 4pm", "Dinner Friday 4pm | Oct 9 | 960 | "),
            ("Dentist Tuesday 3pm", "Dentist Tuesday 3pm | Oct 6 | 900 | "),
            ("DINNER FRIDAY 4PM", "DINNER FRIDAY 4PM | Oct 9 | 960 | "),
            ("Lunch Tomorrow", "Lunch Tomorrow | Oct 6 | - | "),
            ("Dinner FRIDAY 4pm", "Dinner FRIDAY 4pm | Oct 9 | 960 | "),
            ("gym SAT 9AM", "gym SAT 9AM | Oct 10 | 540 | "),
            ("Dinner On Friday", "Dinner | Oct 9 | - | "),
            ("Dinner With Steven On Friday 4PM", "Dinner With Steven | Oct 9 | 960 | Steven"),
        ])
        for title in ["dinner friday 4pm", "Dinner Friday 4pm", "DINNER FRIDAY 4PM", "dinner Friday 4PM"] {
            let r = read(title)
            expect(r.day == day("2026-10-09") && r.start == 960 && r.title == title, "\"\(title)\": Friday, 4pm, as typed")
        }
    }

    /// RULE 3: words come out only when the run is led by on, at or @; an
    /// unled run fills the wells and leaves the title exactly as typed.
    static func rule3OnlyALedRunComesOut() {
        table([
            // Led: the run and its lead come out.
            ("dinner on this friday", "Dinner | Oct 9 | - | "),
            ("dinner with steven on friday 4pm", "Dinner with Steven | Oct 9 | 960 | steven"),
            ("dinner on friday", "Dinner | Oct 9 | - | "),
            ("dinner on next friday", "Dinner | Oct 16 | - | "),
            ("standup at 9am", "Standup | - | 540 | "),
            ("call @ 16:30", "Call | - | 990 | "),
            ("lunch @noon", "Lunch | - | 720 | "),
            ("dinner @7pm", "Dinner | - | 1140 | "),
            ("dinner on friday at 7pm", "Dinner | Oct 9 | 1140 | "),
            ("dinner at 7pm friday", "Dinner | Oct 9 | 1140 | "),
            ("offsite on 10/12", "Offsite | Oct 12 | - | "),
            // Unled: filled, and every word and capital stays.
            ("taco tuesday", "taco tuesday | Oct 6 | - | "),
            ("good friday", "good friday | Oct 9 | - | "),
            ("dinner friday 4pm", "dinner friday 4pm | Oct 9 | 960 | "),
            ("Taco Tuesday", "Taco Tuesday | Oct 6 | - | "),
            ("dinner this friday", "dinner this friday | Oct 9 | - | "),
            ("review next tue", "review next tue | Oct 13 | - | "),
            ("workshop from 4 to 5:30", "workshop from 4 to 5:30 | - | 960-1050 | "),
            // "this", "next" and "from" lead a reading, never a removal; and
            // a run is led by its FIRST word.
            ("dinner friday at 7pm", "dinner friday at 7pm | Oct 9 | 1140 | "),
            // Never an empty title: nothing comes out, the wells still fill.
            ("on friday 4pm", "on friday 4pm | Oct 9 | 960 | "),
            ("at 4pm", "at 4pm | - | 960 | "),
            ("friday 4pm", "friday 4pm | Oct 9 | 960 | "),
            ("on friday at 4pm with steven", "With Steven | Oct 9 | 960 | steven"),
        ])
    }

    /// RULE 4: the run is read whole and consistently, or not at all.
    static func rule4AllOrNothing() {
        untouched([
            // A run with a word in it that cannot be read: no day from it,
            // no time from it (verify8 V8-4's half-read).
            "dinner friday at 7", "dinner friday 7", "dinner friday 7:30", "dinner friday 25pm", "dinner 13pm friday",
            "dinner next next friday", "dinner on on friday", "call 1-800-555 4pm", "table for 4 at 7pm",
            // Two different days, two different times.
            "dinner friday saturday", "dinner friday or saturday", "dinner friday and saturday",
            "trip fri to sun", "dinner fri - sat", "5pm 6pm", "dinner 5pm at 6pm", "dinner this friday next friday",
            "wed tomorrow 4pm",
            // A weekday and a date that disagree.
            "dinner fri oct 10", "dinner sat oct 9", "dinner on fri oct 10 7pm",
            // A range that does not end after it starts.
            "dinner 7pm-6pm", "dinner 4-4pm", "dinner 9am-8am", "dinner 6pm to 6pm", "sync 10am-2am",
            // A year already gone.
            "run oct 12 2000", "run on oct 12 2025 4pm", "dinner on 10/12/2020",
            // Not a date.
            "dinner feb 30", "dinner sep 31",
        ], "rule 4")
        table([
            // The same day or the same time said twice is one day, one time.
            ("dinner friday 4pm 4pm", "dinner friday 4pm 4pm | Oct 9 | 960 | "),
            ("dinner on friday on friday", "Dinner | Oct 9 | - | "),
            ("review fri oct 16", "review fri oct 16 | Oct 16 | - | "),
            // An evening into the next morning is a range.
            ("party 10pm-2am", "party 10pm-2am | - | 1320-120 | "),
            ("party 10-2am", "party 10-2am | - | 1320-120 | "),
            ("dinner 11pm-1am", "dinner 11pm-1am | - | 1380-60 | "),
            // A year that is not over.
            ("planning sept 3 2027", "planning sept 3 2027 | Sep 3 2027 | - | "),
            ("budget dec 1 2026", "budget dec 1 2026 | Dec 1 | - | "),
        ])
    }

    /// RULE 5: for / before / after / until / of / the lead nothing; "by"
    /// fills and keeps the title whole.
    static func rule5SomeWordsLeadNothing() {
        untouched([
            "out of office until friday", "the day after tomorrow", "pray for sun", "study for sat",
            "Edge of Tomorrow", "prep for friday", "closed until monday", "wait until tomorrow", "open until 9pm",
            "work till 5pm", "sleep till noon", "follow up after friday", "finish before monday",
            "dinner the day after tomorrow", "the 1st may 4pm", "table for 4 pm", "pack before sat 9am",
            "meeting of oct 12", "the friday", "dinner 7pm until late",
        ], "rule 5")
        table([
            ("ship v2 by friday", "ship v2 by friday | Oct 9 | - | "),
            ("submit by 5pm", "submit by 5pm | - | 1020 | "),
            ("ship v2 by friday 4pm", "ship v2 by friday 4pm | Oct 9 | 960 | "),
            ("ship v2 by next tue", "ship v2 by next tue | Oct 13 | - | "),
            // "until" between two clocks is a range's own word.
            ("workshop 4pm until 6pm", "workshop 4pm until 6pm | - | 960-1080 | "),
        ])
    }

    /// RULE 6: sun, sat, wed, mar, may, jun and bare numbers.
    static func rule6ShortWordsNeedCompany() {
        untouched([
            "gym sat", "beach sun", "Newly wed", "we wed", "sat", "top 10 may", "dinner may 5", "1 may",
            "notes 5 mar", "dinner jun 3",
            // A number alone is never a clock, led or not; a colon alone is
            // a verse.
            "meet at 4", "email at 9", "dinner at 7", "from 4", "from 3 to 4", "dinner 7", "top 10 at 10",
            "catch 22", "Isaiah 14:12", "Romans 13:11-14", "call 16:30", "launch 4:30", "meeting 9-5",
            "dolly parton 9 to 5", "7-11", "iphone 15",
            // A month needs its day number.
            "may day parade in may", "the march", "see you in march",
        ], "rule 6")
        table([
            // Led.
            ("gym on sat", "Gym | Oct 10 | - | "),
            ("gym this sat", "gym this sat | Oct 10 | - | "),
            ("gym next wed", "gym next wed | Oct 14 | - | "),
            ("dinner on may 5", "Dinner | May 5 2027 | - | "),
            // Beside a plain day or time.
            ("gym sat 9am", "gym sat 9am | Oct 10 | 540 | "),
            ("hike 9am wed", "hike 9am wed | Oct 7 | 540 | "),
            ("hike sat oct 10", "hike sat oct 10 | Oct 10 | - | "),
            ("dinner may 5 7pm", "dinner may 5 7pm | May 5 2027 | 1140 | "),
            ("dinner may 5th", "dinner may 5th | May 5 2027 | - | "),
            // A full month name beside a number is a date; unled, the title
            // keeps every word.
            ("protest march 4", "protest march 4 | Mar 4 2027 | - | "),
            ("offsite oct 12", "offsite oct 12 | Oct 12 | - | "),
            ("offsite 12 oct", "offsite 12 oct | Oct 12 | - | "),
            ("offsite on october 12th", "Offsite | Oct 12 | - | "),
            // A clock says am or pm, or has "at" and a colon.
            ("call at 4:30", "Call | - | 990 | "),
            ("call at 16:30", "Call | - | 990 | "),
            ("standup 9 am", "standup 9 am | - | 540 | "),
            ("call 4 p.m.", "call 4 p.m. | - | 960 | "),
            ("standup 9.15am", "standup 9.15am | - | 555 | "),
        ])
    }

    /// RULE 7 at the parser: a kind that is not taken fills nothing and
    /// loses no words; the kind that is taken still does both.
    static func rule7OnlyAppliedWordsComeOut() {
        let noDay = TitleParse.read("dinner on friday 4pm", today: now, calendar: cal, takeDay: false)
        expect(noDay.day == nil && noDay.start == 960, "the day is not taken, the time is")
        expect(noDay.title == "Dinner on friday", "and \"on friday\" stays in the title: \(noDay.title)")
        let noTime = TitleParse.read("dinner on friday 4pm", today: now, calendar: cal, takeTime: false)
        expect(noTime.day == day("2026-10-09") && noTime.start == nil, "the time is not taken, the day is")
        expect(noTime.title == "Dinner 4pm", "and \"4pm\" stays: \(noTime.title)")
        let neither = TitleParse.read("dinner on friday 4pm", today: now, calendar: cal, takeDay: false, takeTime: false)
        expect(!neither.filled && neither.title == "dinner on friday 4pm", "neither taken: exactly as typed")
        // Rule 4 still holds for a kind that is not taken: a run that
        // cannot be read whole gives nothing to the other kind either.
        let broken = TitleParse.read("dinner fri oct 10 4pm", today: now, calendar: cal, takeDay: false)
        expect(!broken.filled, "a contradicting day is not hidden by the day being the user's")
    }

    /// The reviewers' probes, every one (verify7-E E3 to E8, verify8 V8-4 to
    /// V8-10), with what the eight rules give. Today is Monday October 5.
    static func theProbes() {
        table([
            // verify7 E3
            ("Black Friday sale", "Black Friday sale | - | - | "),
            ("Good Friday service", "Good Friday service | - | - | "),
            ("Taco Tuesday", "Taco Tuesday | Oct 6 | - | "),
            ("Super Bowl Sunday", "Super Bowl Sunday | Oct 11 | - | "),
            ("Thursday Night Football", "Thursday Night Football | - | - | "),
            ("USA Today interview", "USA Today interview | - | - | "),
            ("Tonight Show taping", "Tonight Show taping | - | - | "),
            ("Tomorrow never dies screening", "Tomorrow never dies screening | - | - | "),
            ("Study for SAT", "Study for SAT | - | - | "),
            ("Fun in the sun", "Fun in the sun | - | - | "),
            ("Newly wed", "Newly wed | - | - | "),
            ("prep SAT", "prep SAT | - | - | "),
            ("March 4 Justice rally", "March 4 Justice rally | - | - | "),
            ("Jan 6 hearing", "Jan 6 hearing | - | - | "),
            ("Top 5 Mar picks", "Top 5 Mar picks | - | - | "),
            ("Meet 2 PM candidates", "Meet 2 PM candidates | - | - | "),
            ("Class 5 PM review", "Class 5 PM review | - | - | "),
            ("Psalm 23:10 study", "Psalm 23:10 study | - | - | "),
            ("Isaiah 14:12", "Isaiah 14:12 | - | - | "),
            ("Score 21:17 recap", "Score 21:17 recap | - | - | "),
            ("Notes from 3 to 4 people", "Notes from 3 to 4 people | - | - | "),
            ("Ages from 5 to 12 class", "Ages from 5 to 12 class | - | - | "),
            // verify7 E4
            ("Sunday brunch", "Sunday brunch | - | - | "),
            ("Monday standup", "Monday standup | - | - | "),
            ("Friday prep for launch", "Friday prep for launch | - | - | "),
            // verify7 E5
            ("prep for friday", "prep for friday | - | - | "),
            ("dinner 7pm until late", "dinner 7pm until late | - | - | "),
            ("ship v2 by friday", "ship v2 by friday | Oct 9 | - | "),
            // verify7 E6
            ("dinner fri oct 10", "dinner fri oct 10 | - | - | "),
            ("dinner sat oct 9", "dinner sat oct 9 | - | - | "),
            ("dinner friday 4pm 4pm", "dinner friday 4pm 4pm | Oct 9 | 960 | "),
            ("dinner thursday 9am", "dinner thursday 9am | Oct 8 | 540 | "),
            ("review oct 1 notes", "review oct 1 notes | - | - | "),
            // verify7 E7
            ("dinner with steven", "dinner with steven | - | - | steven"),
            ("dinner with steven friday", "dinner with steven friday | Oct 9 | - | steven"),
            ("walk with dog 4pm", "walk with dog 4pm | - | 960 | dog"),
            ("sync with marketing team friday", "sync with marketing team friday | Oct 9 | - | marketing team"),
            ("coffee with you know who 4pm", "coffee with you know who 4pm | - | 960 | you know"),
            ("dinner with o'brien fri 6pm", "dinner with o'brien fri 6pm | Oct 9 | 1080 | o'brien"),
            // verify7 E8, as titles
            ("dinner friday 7pm", "dinner friday 7pm | Oct 9 | 1140 | "),
            ("dinner friday", "dinner friday | Oct 9 | - | "),
            ("dinner on friday at 7pm", "Dinner | Oct 9 | 1140 | "),
            // verify8 V8-4
            ("Dinner Friday 4pm", "Dinner Friday 4pm | Oct 9 | 960 | "),
            ("Dentist Tuesday 3pm", "Dentist Tuesday 3pm | Oct 6 | 900 | "),
            ("Flight Oct 12 4pm", "Flight Oct 12 4pm | Oct 12 | 960 | "),
            ("Standup Mon 9am", "Standup Mon 9am | Oct 5 | 540 | "),
            ("Pick up Sam Friday 3pm", "Pick up Sam Friday 3pm | Oct 9 | 900 | "),
            ("Dr. Lee Friday 2pm", "Dr. Lee Friday 2pm | Oct 9 | 840 | "),
            ("Dinner at Nobu Friday 7pm", "Dinner at Nobu Friday 7pm | Oct 9 | 1140 | "),
            ("DINNER FRIDAY 4PM", "DINNER FRIDAY 4PM | Oct 9 | 960 | "),
            ("Dinner Friday", "Dinner Friday | Oct 9 | - | "),
            ("Lunch Friday", "Lunch Friday | Oct 9 | - | "),
            ("Lunch Tomorrow", "Lunch Tomorrow | Oct 6 | - | "),
            ("Dinner Tonight", "Dinner Tonight | Oct 5 | - | "),
            ("Dinner Oct 12", "Dinner Oct 12 | Oct 12 | - | "),
            ("Hearing Jan 6", "Hearing Jan 6 | Jan 6 2027 | - | "),
            ("Call Mom Sunday", "Call Mom Sunday | Oct 11 | - | "),
            ("Trip to Paris Friday", "Trip to Paris Friday | Oct 9 | - | "),
            ("Review Q3 Friday", "Review Q3 Friday | Oct 9 | - | "),
            ("dinner Friday 4pm", "dinner Friday 4pm | Oct 9 | 960 | "),
            ("Dinner friday 4pm", "Dinner friday 4pm | Oct 9 | 960 | "),
            ("Dinner On Friday", "Dinner | Oct 9 | - | "),
            ("Lunch with Ana Friday", "Lunch with Ana Friday | Oct 9 | - | Ana"),
            // verify8 V8-5
            ("taco tuesday", "taco tuesday | Oct 6 | - | "),
            ("good friday", "good friday | Oct 9 | - | "),
            ("cyber monday", "cyber monday | Oct 5 | - | "),
            ("casual friday", "casual friday | Oct 9 | - | "),
            ("palm sunday", "palm sunday | Oct 11 | - | "),
            ("ash wednesday", "ash wednesday | Oct 7 | - | "),
            ("super bowl sunday", "super bowl sunday | Oct 11 | - | "),
            ("usa today", "usa today | Oct 5 | - | "),
            ("read usa today", "read usa today | Oct 5 | - | "),
            ("edge of tomorrow", "edge of tomorrow | - | - | "),
            ("Edge of Tomorrow", "Edge of Tomorrow | - | - | "),
            ("thank god its friday", "thank god its friday | Oct 9 | - | "),
            ("not today", "not today | Oct 5 | - | "),
            // verify8 V8-6
            ("Never on Sunday screening", "Never on Sunday screening | - | - | "),
            ("Brunch on Sunday Funday", "Brunch on Sunday Funday | - | - | "),
            ("turn on friday mode", "turn on friday mode | - | - | "),
            ("The next friday movie", "The next friday movie | - | - | "),
            ("this monday morning podcast", "this monday morning podcast | - | - | "),
            ("Vote on 1/2 measure", "Vote on 1/2 measure | - | - | "),
            ("Notes on 1/2 marathon", "Notes on 1/2 marathon | - | - | "),
            ("work on 3/4 time piece", "work on 3/4 time piece | - | - | "),
            ("Look at 4:30 report", "Look at 4:30 report | - | - | "),
            ("Meet at 3:16 pub", "Meet at 3:16 pub | - | - | "),
            ("Arrive at midnight oil show", "Arrive at midnight oil show | - | - | "),
            ("Rate from 08:00 feed", "Rate from 08:00 feed | - | - | "),
            ("stare at noon sun", "Stare | Oct 11 | 720 | "),
            ("dinner on this friday", "Dinner | Oct 9 | - | "),
            ("dinner on next friday", "Dinner | Oct 16 | - | "),
            ("dinner next next friday", "dinner next next friday | - | - | "),
            ("dinner / friday", "dinner / friday | Oct 9 | - | "),
            // verify8 V8-7
            ("dinner the day after tomorrow", "dinner the day after tomorrow | - | - | "),
            ("the day before friday", "the day before friday | - | - | "),
            ("out of office until friday", "out of office until friday | - | - | "),
            ("closed until monday", "closed until monday | - | - | "),
            ("wait until tomorrow", "wait until tomorrow | - | - | "),
            ("pray for sun", "pray for sun | - | - | "),
            ("look for sun hat", "look for sun hat | - | - | "),
            ("time for sun", "time for sun | - | - | "),
            ("study for sat", "study for sat | - | - | "),
            ("Prep for sat", "Prep for sat | - | - | "),
            ("gift for wed", "gift for wed | - | - | "),
            // verify8 V8-8
            ("protest march 4", "protest march 4 | Mar 4 2027 | - | "),
            ("Women's march 21", "Women's march 21 | Mar 21 2027 | - | "),
            ("chapter 7 march", "chapter 7 march | Mar 7 2027 | - | "),
            ("catch 22 june", "catch 22 june | Jun 22 2027 | - | "),
            ("top 10 may", "top 10 may | - | - | "),
            ("take 5 dec", "take 5 dec | Dec 5 | - | "),
            ("the 1st may", "the 1st may | - | - | "),
            ("run oct 12 2000", "run oct 12 2000 | - | - | "),
            ("windows 11 pm", "windows 11 pm | - | 1380 | "),
            ("ocean's 11 pm", "ocean's 11 pm | - | 1380 | "),
            ("who 10 am", "who 10 am | - | 600 | "),
            ("ratio 4 to 1 pm", "ratio 4 to 1 pm | - | 240-780 | "),
            ("dinner 4-4pm", "dinner 4-4pm | - | - | "),
            ("dinner 7pm-6pm", "dinner 7pm-6pm | - | - | "),
            // verify8 V8-9
            ("dinner friday with steven", "dinner friday with steven | Oct 9 | - | steven"),
            ("lunch tomorrow with ana", "lunch tomorrow with ana | Oct 6 | - | ana"),
            ("coffee 4pm with bob", "coffee 4pm with bob | - | 960 | bob"),
            ("dinner friday 4pm with steven", "dinner friday 4pm with steven | Oct 9 | 960 | steven"),
            ("dinner on friday with steven", "Dinner with Steven | Oct 9 | - | steven"),
            // verify8 V8-10, as titles
            ("lunch friday 1pm", "lunch friday 1pm | Oct 9 | 780 | "),
            ("lunch saturday", "lunch saturday | Oct 10 | - | "),
            ("dinner friday 8pm", "dinner friday 8pm | Oct 9 | 1200 | "),
        ])
    }

    static func ranges() {
        table([
            ("workshop 4-6pm", "workshop 4-6pm | - | 960-1080 | "),
            ("workshop 4pm-6pm", "workshop 4pm-6pm | - | 960-1080 | "),
            ("workshop 11-1pm", "workshop 11-1pm | - | 660-780 | "),
            ("workshop 9-11am", "workshop 9-11am | - | 540-660 | "),
            ("workshop 4 to 6pm", "workshop 4 to 6pm | - | 960-1080 | "),
            ("workshop 4pm to 6", "workshop 4pm to 6 | - | 960-1080 | "),
            ("workshop at 4:30-5", "Workshop | - | 990-1020 | "),
            ("workshop friday from 4 to 5:30", "workshop friday from 4 to 5:30 | Oct 9 | 960-1050 | "),
            ("meeting 9-5pm", "meeting 9-5pm | - | 540-1020 | "),
        ])
    }

    static func days() {
        expect(read("lunch today").day == day("2026-10-05") && read("drinks tonight").day == day("2026-10-05"), "today, tonight")
        expect(read("lunch tmrw").day == day("2026-10-06"), "tmrw")
        expect(read("review this thu").day == day("2026-10-08"), "this thu")
        expect(read("review on mon").day == day("2026-10-05"), "a weekday that is today is today")
        expect(read("planning sept 3 2027").day == day("2027-09-03"), "a year, when one is said")
        // "next tue" is next WEEK's, by the user's own weeks. On Sunday Oct 4:
        // Sunday-first weeks put it on the 13th, Monday-first on the 6th.
        let sunday = at("2026-10-04T10:00:00-07:00")
        expect(read("review next tue", calendar(firstWeekday: 1), today: sunday).day == day("2026-10-13"),
               "next tue from a Sunday, weeks from Sunday: the 13th")
        expect(read("review next tue", calendar(firstWeekday: 2), today: sunday).day == day("2026-10-06"),
               "weeks from Monday: the 6th")
        expect(read("review next mon").day == day("2026-10-12"), "next mon is never today")
        expect(read("").title == "" && read("   ").title == "", "an empty title is an empty title")
        untouched(["May day parade", "lunch at Friday's", "Q4 planning", "5k run", "room 4a", "version 4.30 review",
                   "wedding plans", "sunny side up brunch", "midnight mass rehearsal", "high noon screening",
                   "next steps", "this is it", "1930s film night", "on 10/12/", "10/12 retro", "follow-up",
                   "dinner (friday)", "dinner fridays", "at", "on"], "not a day")
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
        expect(read("dinner friday 4pm with steven and ana").names == ["steven", "ana"], "names after the day and time")
    }

    /// The title that is saved when words DO come out (a led run).
    static func theSavedTitle() {
        expect(read("Dinner with Steven on Friday 4pm").title == "Dinner with Steven", "capitals typed are kept")
        // The capital is decided by what is kept (verify7 E4).
        expect(read("lunch on Friday").title == "Lunch", "the only capital came out with the day: \(read("lunch on Friday").title)")
        expect(read("dinner with Steven on friday").title == "dinner with Steven",
               "one capital typed anywhere: the first letter is left as typed")
        expect(read("dinner - on friday 7pm").title == "Dinner", "a dash left hanging is dropped")
        expect(read("dinner: on friday").title == "Dinner", "and a colon")
        expect(read("dinner  with   steven   on friday").title == "Dinner with Steven", "spaces are single")
        expect(read("  dinner on friday  ").title == "Dinner", "and trimmed")
        expect(read("  dinner friday  ").title == "dinner friday", "an unled title is only trimmed")
        expect(read("école on friday").title == "École", "the capital is the letter's own")
        expect(read("Dinner on Friday?").title == "Dinner", "a question mark goes with the day")
        expect(!read("dinner with steven on friday 4pm").title.contains("\u{2014}"), "no em dash")
        // A NAME GETS ITS CAPITAL, when words come out and it was typed
        // without one.
        expect(read("dinner with steven and ana on friday").title == "Dinner with Steven and Ana",
               "each lower-case name is capitalised, and \"and\" is not a name")
        expect(read("dinner with steven ng, ana & bo on friday").title == "Dinner with Steven Ng, Ana & Bo",
               "two-word names, a comma and an ampersand")
        expect(read("dinner with mary-jane on friday").title == "Dinner with Mary-Jane", "both halves of a hyphened name")
        expect(read("dinner with sTeven on friday").title == "dinner with sTeven", "a mixed-case name is the user's own")
        expect(read("dinner with DJ on friday").title == "dinner with DJ", "and so is one in capitals")
        expect(read("lunch with McKay and ana on friday").title == "lunch with McKay and Ana",
               "each name on its own: the typed capital is kept, the lower-case one gets its own")
        expect(read("sync with the team on friday").title == "Sync with the team", "words that are not names are left alone")
        expect(read("dinner for steven on friday").title == "Dinner for steven", "only a name after \"with\" is one")
        // Nothing came out: exactly as typed, names and all (verify7 E7:
        // the same title with and without a day).
        expect(read("dinner with steven").title == "dinner with steven", "no day: as typed")
        expect(read("dinner with steven friday").title == "dinner with steven friday", "an unled day: as typed")
        expect(read("dinner with steven friday").names == ["steven"], "the name asked about is the one typed")
    }

    // MARK: the title in the form

    static func saved(_ d: EventDraft) -> String { EventForm.savedTitle(d, today: now, calendar: cal) }
    static func savesChanged(_ d: EventDraft) -> Bool { EventForm.titleSavesChanged(d, today: now, calendar: cal) }

    /// RULE 7 in the form (verify7 E8, verify8 V8-10): a well set by hand
    /// wins, and only words whose reading is IN a well are out of the title.
    static func rule7AHandSetWellWins() {
        // verify8 V8-10: a day clicked on the grid, then the day word typed
        // again. It was removed AND ignored; now it is neither.
        let clicked = EventForm.moved(typed(fresh(), "dinner on friday"), to: day("2026-10-20"), today: now, calendar: cal)
        expect(clicked.date == "Tue Oct 20", "the clicked day is the day")
        expect(saved(clicked) == "dinner on friday", "and \"on friday\", which is in no well, is back in the title: \(saved(clicked))")
        let retitled = typed(clicked, "lunch on friday at 1pm")
        expect(retitled.date == "Tue Oct 20" && retitled.start == "1pm", "retitled: Tuesday still, and the 1pm is read")
        expect(saved(retitled) == "Lunch on friday", "\"at 1pm\" filled a well and comes out; \"on friday\" filled none and stays: \(saved(retitled))")
        expect(saved(typed(clicked, "lunch on saturday")) == "lunch on saturday", "any day word, the same or another, stays")
        // The unled spellings never lose a word either way.
        let plain = EventForm.moved(typed(fresh(), "dinner friday"), to: day("2026-10-20"), today: now, calendar: cal)
        expect(saved(plain) == "dinner friday" && saved(typed(plain, "lunch friday 1pm")) == "lunch friday 1pm",
               "unled: as typed before and after the click")
        expect(typed(plain, "lunch friday 1pm").start == "1pm" && typed(plain, "lunch friday 1pm").date == "Tue Oct 20",
               "and the time beside the hand-set day is still read")

        // verify7 E8: the END corrected. The start is still the title's, so
        // its words stay out.
        let base = typed(fresh(), "dinner on friday at 7pm")
        expect(saved(base) == "Dinner" && base.start == "7pm" && base.end == "8pm", "the title as first read")
        var ended = base
        ended.end = "9:30pm"
        ended = EventForm.handSet(ended, .end)
        expect(ended.titleRead.hand == [.end], "only the end is the user's")
        expect(ended.start == "7pm" && ended.end == "9:30pm" && saved(ended) == "Dinner",
               "\"at 7pm\" does not come back into the title: \(saved(ended))")
        expect(typed(ended, "dinner on friday at 7pm ").end == "9:30pm", "the hand-set end is never overwritten")
        // verify8 V8-10 B3: then the title's time is edited. It was left in
        // the title and ignored; now the start follows it.
        let later = typed(ended, "dinner on friday at 8pm")
        expect(later.start == "8pm" && later.end == "9:30pm" && saved(later) == "Dinner",
               "a new time in the title moves the start it still owns: \(later.start), \(saved(later))")
        expect(typed(ended, "dinner").start == "4pm" && typed(ended, "dinner").end == "9:30pm",
               "deleting the words puts back the start, and leaves the end the hand set")
        // The START corrected: both times are the user's, and the time words
        // are back in the title, since they are in no well.
        var started = base
        started.start = "8pm"
        started = EventForm.handSet(started, .start)
        expect(started.titleRead.hand == [.start, .end], "a start takes its end with it")
        expect(saved(started) == "Dinner at 7pm" && started.date == "Fri Oct 9",
               "the day is still the title's and stays out; the time is the user's and stays in: \(saved(started))")
        // An end typed beside a start the title did NOT write is a span the
        // user set: a time typed later in the title moves neither end.
        var span = fresh()
        span.end = "6pm"
        span = EventForm.handSet(span, .end)
        expect(span.titleRead.hand == [.start, .end], "an end beside the form's own start takes the start too")
        let named = typed(span, "dinner on friday at 7pm")
        expect(named.start == "4pm" && named.end == "6pm" && named.date == "Fri Oct 9", "the span stays, the day is read")
        expect(saved(named) == "Dinner at 7pm", "and the time's words stay in the title: \(saved(named))")
        // A well set by hand BEFORE the title said anything.
        var first = fresh()
        first.date = "Oct 20"
        first = EventForm.handSet(first, .date)
        expect(saved(typed(first, "dinner on friday")) == "dinner on friday", "nothing was filled, so nothing comes out")
    }

    /// RULE 8: the "saves as" line is for a saved title that is not the
    /// typed one, and for nothing else.
    static func rule8TheSavesAsLine() {
        for title in ["dinner on friday", "dinner with steven on friday 4pm", "lunch at noon", "Dinner On Friday"] {
            let d = typed(fresh(), title)
            expect(savesChanged(d), "\"\(title)\": words come out, so the line shows")
            expect(EventForm.titleIsRead(d, today: now, calendar: cal), "and Esc has something to undo")
        }
        for title in ["dinner friday 4pm", "taco tuesday", "ship v2 by friday", "Dinner Friday", "on friday 4pm",
                      "dinner with steven friday"] {
            let d = typed(fresh(), title)
            expect(!savesChanged(d), "\"\(title)\": saved as typed, so no line")
            expect(!d.titleRead.filled.isEmpty, "the wells are what show it was read")
            expect(EventForm.titleIsRead(d, today: now, calendar: cal), "and Esc still puts the wells back")
        }
        for title in ["dinner", "Black Friday sale", "dinner with steven", "  dinner  ", ""] {
            expect(!savesChanged(typed(fresh(), title)), "\"\(title)\": nothing read, no line")
        }
        // Literal, an edit, a hand-set day: the saved title is the typed one.
        let literal = EventForm.literalTitle(typed(fresh(), "dinner on friday"), today: now, calendar: cal)
        expect(!savesChanged(literal), "kept as typed: no line")
        let clicked = EventForm.moved(typed(fresh(), "dinner on friday"), to: day("2026-10-20"), today: now, calendar: cal)
        expect(!savesChanged(clicked), "the day set by hand: nothing comes out, no line")
        var edit = EventDraft()
        (edit.title, edit.calendarId, edit.date, edit.start, edit.end) = ("Dinner on Friday", "primary", "Thu Oct 8", "7pm", "8pm")
        expect(!savesChanged(edit), "an edit is never read")
    }

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
        expect(saved(d) == "Dinner with Steven", "the saved title")
        // Unled: the same wells, the title as typed.
        let unled = typedByLetter(base, "dinner friday 7pm with steven")
        expect(unled.date == "Fri Oct 9" && unled.start == "7pm" && unled.end == "8pm", "the other order fills the same wells")
        expect(Set(unled.titleRead.filled.keys) == [.date, .start, .end] && saved(unled) == "dinner friday 7pm with steven",
               "washed as the title's, and saved as typed")
        let ranged = typed(base, "workshop friday 2-4pm")
        expect(ranged.start == "2pm" && ranged.end == "4pm", "a range fills both ends")
        let overnight = typed(base, "party sat 10pm-2am")
        expect(overnight.start == "10pm" && overnight.end == "2am", "an overnight range as it is said")
        if case .success(let w) = EventForm.when(overnight, today: now, calendar: cal) {
            expect(EventForm.summary(w, now: now, calendar: cal) == "Sat Oct 10, 10pm to 2am Sun", "and it ends the next day")
        } else {
            expect(false, "the overnight draft resolves")
        }
        // Idempotent: the same title again changes nothing.
        expect(typed(d, d.title) == d, "typing nothing new changes nothing")
        // Letter by letter and back again: the blank draft (verify7 E, held).
        var back = d
        var text = d.title
        while !text.isEmpty {
            text.removeLast()
            back = typed(back, text)
        }
        expect(back == base, "typed a letter at a time and deleted a letter at a time: the draft it started as")
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
        // A run that stops reading whole un-fills all of it (rule 4).
        let broken = typed(filled, "dinner friday 7pm 8pm")
        expect(broken.date == "Mon Oct 5" && broken.start == "4pm", "a second, different time: everything goes back")
    }

    static func aHandSetWellIsNeverOverwritten() {
        var d = fresh()
        // The user types a day into the well themselves.
        d.date = "Oct 20"
        d = EventForm.handSet(d, .date)
        d = typedByLetter(d, "dinner on friday 7pm")
        expect(d.date == "Oct 20", "a day set by hand survives a day typed in the title")
        expect(d.start == "7pm", "the time, not set by hand, is still filled")
        expect(saved(d) == "Dinner on friday", "and \"on friday\" stays in the title, since it filled nothing: \(saved(d))")
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
        let named = typed(swept, "sync with ana on friday 3pm")
        expect(named.date == "Wed Oct 7" && named.start == "10am", "a swept span is never moved by its title")
        expect(EventForm.titleReading(named, today: now, calendar: cal)?.names == ["ana"], "but who it is with is read")
        expect(saved(named) == "sync with ana on friday 3pm", "and its title is saved as typed")
    }

    static func escTakesTheTitleAsTyped() {
        for title in ["dinner on friday 7pm", "dinner friday 7pm"] {
            let d = typed(fresh(), title)
            expect(EventForm.titleIsRead(d, today: now, calendar: cal), "\"\(title)\": there is something for Esc to undo")
            let literal = EventForm.literalTitle(d, today: now, calendar: cal)
            expect(literal.title == title, "the typed words stay in the field")
            expect(literal.date == "Mon Oct 5" && literal.start == "4pm" && literal.end == "5pm",
                   "and everything they filled goes back")
            expect(saved(literal) == title, "the title is saved as typed")
            expect(!EventForm.titleIsRead(literal, today: now, calendar: cal), "a second Esc has nothing to undo here")
            expect(typed(literal, title + " sharp").date == "Mon Oct 5", "more typing is still literal")
            let again = typed(typed(literal, ""), "lunch friday")
            expect(again.date == "Fri Oct 9", "an emptied title is read afresh")
        }
        expect(!EventForm.titleIsRead(typed(fresh(), "dinner with steven"), today: now, calendar: cal),
               "a title that filled nothing leaves Esc to close the pane")
    }

    static func anEditIsNeverRead() {
        var d = EventDraft()
        (d.title, d.calendarId, d.date, d.start, d.end) = ("Dinner on Friday 4pm", "primary", "Thu Oct 8", "7pm", "8pm")
        expect(!d.titleRead.on, "a draft that was not made new is not read")
        let renamed = typed(d, "Dinner on Saturday 5pm")
        expect(renamed.date == "Thu Oct 8" && renamed.start == "7pm", "an edit's title moves nothing")
        expect(saved(renamed) == "Dinner on Saturday 5pm", "and is saved as typed")
        expect(EventForm.titleReading(renamed, today: now, calendar: cal) == nil, "no reading, so no suggestion")
    }

    static func allDayTakesNoTime() {
        var d = fresh()
        d.allDay = true
        d = typed(d, "offsite on friday 4pm")
        expect(d.date == "Fri Oct 9", "an all-day event takes the day")
        expect(d.start == "4pm" && d.titleRead.filled[.start] == nil, "and no time: the wells it hides are left")
        expect(saved(d) == "Offsite 4pm", "so \"4pm\", which filled nothing, stays in the title: \(saved(d))")
        expect(saved(typed(d, "offsite friday 4pm")) == "offsite friday 4pm", "unled: as typed")
    }

    static func theSaveUsesTheSavedTitle() {
        let d = typed(fresh(), "dinner with steven on friday 7pm")
        guard case .success(let body) = EventForm.wire(d, today: now, calendar: cal) else {
            expect(false, "the read draft saves")
            return
        }
        expect(body.title == "Dinner with Steven", "the body carries the saved title: \(body.title)")
        expect(body.start == "2026-10-09T19:00:00-07:00" && body.end == "2026-10-09T20:00:00-07:00",
               "on the day and at the time it named")
        expect(body.guests == nil, "and names nobody: a name in the title is not a guest")
        let chip = EventForm.provisional(d, color: nil, today: now, calendar: cal)
        expect(chip?.title == "Dinner with Steven", "the grid's chip shows the saved title")
        // Unled: the typed title is the saved one, on the day it named.
        if case .success(let unled) = EventForm.wire(typed(fresh(), "Taco Tuesday 6pm"), today: now, calendar: cal) {
            expect(unled.title == "Taco Tuesday 6pm" && unled.start == "2026-10-06T18:00:00-07:00",
                   "an unled title is saved whole, where it said: \(unled.title) \(unled.start)")
        } else {
            expect(false, "the unled draft saves")
        }
        // A title that is only a day and a time keeps its words: it is never
        // emptied, so it is never "needs a title".
        let only = typed(fresh(), "on friday 7pm")
        expect(saved(only) == "on friday 7pm" && EventForm.problem(only, today: now, calendar: cal) == nil,
               "nothing but a day and a time is its own title")
        let long = typed(fresh(), String(repeating: "a", count: 200) + " on friday")
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
            case .success(let w): return EventForm.summary(w, now: now, calendar: cal)
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
        let d = typed(fresh(), "dinner with steven and ana on friday")
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
        let d = typed(fresh(), "dinner with steven and ana on friday")
        let people = ["steven": [steven, stevenP], "ana": [ana]]
        // The checkmark.
        let added = EventGuests.accepting(steven, for: "steven", in: d)
        expect(EventGuests.read(added.guests).addresses == ["steven@example.com"], "the checkmark makes a real guest")
        expect(EventGuests.read(added.guests).unread.isEmpty && added.guests.hasSuffix(","), "as a whole pill")
        expect(offer(added, people: people)?.name == "ana", "and the next name is offered")
        expect(added.title == d.title, "the checkmark does not touch the typed title")
        expect(EventForm.savedTitle(added, today: now, calendar: cal) == EventForm.savedTitle(d, today: now, calendar: cal)
            && EventForm.savedTitle(added, today: now, calendar: cal) == "Dinner with Steven and Ana",
               "nor rewrite the saved one: \"Steven\" does not become the contact's \"Steven Ng\"")
        expect(added.date == d.date && added.start == d.start && added.end == d.end, "nor the when")
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
        // The daemon refuses a name under two LETTERS (verify7 D1, verify8
        // V8-13: letters and digits of the first word, as its store reads
        // the name), so none is asked.
        expect(EventGuests.queries(for: "s").isEmpty, "one letter is not asked")
        expect(EventGuests.queries(for: "s ng").isEmpty, "nor is a first word of one letter with a second after it")
        expect(EventGuests.queries(for: "st") == ["st"], "two letters are a name")
        for short in ["s.", "s-", "(s)", "s'", "o'", "j-", ". s", "s .", "--", "'", "é"] {
            expect(EventGuests.queries(for: short).isEmpty, "\"\(short)\" is one letter at most: not asked")
            expect(!EventGuests.askable(short), "\"\(short)\" is not askable")
        }
        for long in ["st", "(st)", "o'b", "st.", "j2", "steven ng", "al", "李明"] {
            expect(EventGuests.askable(long), "\"\(long)\" has two letters: askable")
        }
        expect(EventGuests.queries(for: "o'brien") == ["o'brien"], "an apostrophe inside a name is the name's")
        // And the title does not take a one-letter word as a name at all.
        expect(read("dinner with o' friday").names.isEmpty && read("dinner with j- friday").names.isEmpty,
               "\"o'\" and \"j-\" after \"with\" are not names")
        expect(read("dinner with al friday").names == ["al"] && read("dinner with o'brien").names == ["o'brien"],
               "two letters are, and so is O'Brien")
        expect(EventGuests.nameKey(" Steven  NG ") == "steven ng", "a name's key is folded and single-spaced")
    }

    /// verify7 E9: a lookup that failed is not remembered as "nobody".
    static func aFailedLookupIsNotAnAnswer() {
        typealias Ask = EventGuests.NameAsk
        expect(EventGuests.nameAnswer([.people([steven])]) == [steven], "somebody found is the answer")
        expect(EventGuests.nameAnswer([.people([]), .people([ana])]) == [ana], "the second question's, when the first found nobody")
        expect(EventGuests.nameAnswer([.people([])]) == [], "asked, nobody: remembered")
        expect(EventGuests.nameAnswer([.people([]), .people([])]) == [], "both asked, nobody")
        expect(EventGuests.nameAnswer([.failed]) == nil, "a failure is no answer: nothing is remembered")
        expect(EventGuests.nameAnswer([.people([]), .failed]) == nil, "nobody and then a failure is still no answer")
        expect(EventGuests.nameAnswer([.failed, .people([])]) == nil, "a failure and then nobody: the failed question might have found them")
        expect(EventGuests.nameAnswer([.failed, .people([steven])]) == [steven], "somebody found after a failure is an answer")
        expect(EventGuests.nameAnswer([.noRoute]) == [], "a daemon without the route is nobody, for good")
        expect(EventGuests.nameAnswer([.failed, .noRoute]) == [], "and so after a failure")
        expect(EventGuests.nameAnswer([]) == [], "nothing to ask is nobody")
        expect(EventGuests.nameSettled(by: Ask.people([steven])) && EventGuests.nameSettled(by: Ask.noRoute),
               "the questions stop at somebody, or at no route")
        expect(!EventGuests.nameSettled(by: Ask.people([])) && !EventGuests.nameSettled(by: Ask.failed),
               "and go on past nobody and past a failure")
        // The pane's own bookkeeping: an unanswered name is still missing.
        var remembered: [String: [NamedPerson]] = [:]
        if let a = EventGuests.nameAnswer([.failed]) { remembered["steven"] = a }
        expect(remembered["steven"] == nil, "so it is asked again")
        let d = typed(fresh(), "dinner with steven friday")
        expect(offer(d, people: remembered) == nil, "and nothing is offered meanwhile")
    }

    /// verify7 E10: where Tab goes, and what ↵ may add.
    static func theKeyboardAndTheSuggestion() {
        let settle = EventGuests.suggestionSettle
        expect(!EventGuests.tabTakesSuggestion(caretInTitle: true, shownFor: nil), "nothing offered: Tab is the fields' own")
        expect(!EventGuests.tabTakesSuggestion(caretInTitle: true, shownFor: 0),
               "an answer landing as Tab is pressed does not move where Tab goes")
        expect(!EventGuests.tabTakesSuggestion(caretInTitle: true, shownFor: settle - 0.01), "nor just before it has settled")
        expect(EventGuests.tabTakesSuggestion(caretInTitle: true, shownFor: settle), "a box that has been there takes Tab")
        expect(!EventGuests.tabTakesSuggestion(caretInTitle: false, shownFor: 60),
               "with the caret already past the title, never")
        expect(settle >= 0.5, "long enough to be seen")
        // The keyboard stays on the box only for the same offer.
        let d = typed(fresh(), "dinner with steven and ana on friday")
        let people = ["steven": [steven, stevenP], "ana": [ana]]
        let first = offer(d, people: people)
        let second = offer(EventGuests.accepting(steven, for: "steven", in: d), people: people)
        expect(first?.name == "steven" && second?.name == "ana", "one name answered, the next offered")
        expect(EventGuests.keyboardStays(was: first, now: first), "the same offer keeps the keyboard")
        expect(!EventGuests.keyboardStays(was: first, now: second), "the next name's does not: Return must not add Ana unasked")
        expect(!EventGuests.keyboardStays(was: first, now: nil), "an offer that went does not")
        expect(!EventGuests.keyboardStays(was: nil, now: first), "one that just arrived never has it")
        let swapped = offer(d, people: ["steven": [stevenP, steven], "ana": [ana]])
        expect(!EventGuests.keyboardStays(was: first, now: swapped), "nor the same name with another person in front")
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
