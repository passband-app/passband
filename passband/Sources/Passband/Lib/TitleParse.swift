// A TITLE THAT UNDERSTANDS "dinner with steven on friday 4pm" (wave 6, the
// founder: "when i type the title ... it should know to populate the date and
// person").
//
// NO MODEL. A deterministic reader over the words of the title, built on the
// event form's own date and time parsers (Lib/EventDraft.swift), so "fri" and
// "7:45pm" mean here exactly what they mean in the wells.
//
// PURE, and Foundation only (Tests/TitleParseTests.swift). Clock and zone are
// arguments.
//
// THE RULES. Eight, and no special cases beside them.
//
//   1. ONLY THE END OF THE TITLE IS READ. A closing "with <somebody>" is set
//      aside first, so "dinner friday 4pm with steven" and "dinner with
//      steven friday 4pm" both work. What is read is the TRAILING RUN: every
//      last word that looks like a day or a time word, or one of their leads
//      (on, at, @, this, next, from, to). A day or a time anywhere before the
//      run is a word of the title: "Black Friday sale", "Never on Sunday
//      screening", "Meet 2 PM candidates", "Vote on 1/2 measure". The word
//      after "with" is never a day or a time ("call with friday").
//   2. CAPITALS DO NOT MATTER in the run: "Dinner Friday 4pm", "DINNER FRIDAY
//      4PM" and "Lunch Tomorrow" read as their lower-case selves do.
//   3. WORDS COME OUT OF THE SAVED TITLE ONLY WHEN THE RUN IS LED by on, at
//      or @: "dinner on this friday" is saved as "Dinner", "dinner with
//      steven on friday 4pm" as "Dinner with Steven". An unled run fills the
//      wells and leaves the title EXACTLY as typed: "taco tuesday" is on
//      Tuesday and is still called "taco tuesday". Nothing is ever taken out
//      that would leave no title.
//   4. ALL OR NOTHING. The run is read whole or not at all: never a time
//      without the day beside it, never a day from a run whose time cannot
//      be read. Two different days, two different times, a weekday and a
//      date that disagree ("fri oct 10", a Saturday), a range that does not
//      end after it starts ("7pm-6pm", "4-4pm"; an evening into the next
//      morning, "10pm-2am", is a range), a year already gone: nothing.
//   5. for / before / after / until / of / the LEAD NOTHING: "out of office
//      until friday", "the day after tomorrow", "pray for sun", "Edge of
//      Tomorrow". "by friday" is an unled run like any other: it fills the
//      day and the title stays whole.
//   6. SHORT WORDS THAT ARE ALSO WORDS (sun, sat, wed, mar, may, jun) are
//      read only when led (on, this, next) or when the day or time beside
//      them in the run is a plain one. "gym sat" is not read; "gym sat 9am"
//      and "gym on sat" are. A month needs its day number, and a full month
//      name beside a number is a date: "protest march 4" fills March 4 and,
//      unled, keeps its title. A NUMBER ALONE IS NEVER A CLOCK, led or not:
//      the form reads a bare 7 as 7am, which is not what "dinner at 7"
//      means, and "meet at 4" may be a room. A clock says am or pm ("4pm"),
//      or has a lead and a colon ("at 4:30"); a colon alone is a verse or a
//      score ("Isaiah 14:12").
//   7. A WELL SET BY HAND WINS, and nothing comes out of the title for it:
//      only words whose reading was put in a well are ever removed
//      (`takeDay` / `takeTime`).
//   8. THE "saves as" LINE under the title shows only when the saved title
//      is not the typed one (`EventForm.titleSavesChanged`). An unled
//      reading shows as the wash on the wells it filled.
//
// THE SAVED TITLE, when words come out: the "with <name>" words stay, a name
// typed in lower case gets its capital ("Dinner with Steven"), and a title
// left with no capital at all gets one on its first letter.

import Foundation

/// What a title said, beside its own words.
struct TitleReading: Hashable, Sendable {
    /// The title to save: the typed one less the words that named the day
    /// and the time.
    var title: String
    /// Local midnight of the day named, if one was.
    var day: Date?
    /// Minutes after midnight, if a time was named.
    var start: Int?
    /// The end of a stated range; nil when only a start was said.
    var end: Int?
    /// Who it is with, as typed, in order, each at most two words.
    var names: [String]

    /// Words came out of the title for a day or a time.
    var filled: Bool { day != nil || start != nil }
}

enum TitleParse {
    /// Names one title may carry. Past it the rest are words.
    static let maxNames = 4

    /// One day or time expression of the trailing run.
    private struct Phrase {
        /// Its words, the lead-in included.
        var range: Range<Int>
        var isDay: Bool
        var day: Date?
        var start = 0
        var end: Int?
        /// Plain ("friday", "4pm", "oct 12"), or led. A phrase that is not
        /// ("sat", "may 5") is read only beside one that is.
        var sure: Bool
    }

    /// Read a title. `takeDay` / `takeTime` false leaves that kind of word in
    /// the title and out of the reading: the pane passes false for a field
    /// the user has set by hand, so words that would not fill anything are
    /// not taken out of the title either.
    static func read(
        _ raw: String, today: Date, calendar: Calendar, takeDay: Bool = true, takeTime: Bool = true
    ) -> TitleReading {
        let words = raw.split(whereSeparator: \.isWhitespace).map(String.init)
        let low = words.map(normal)

        // WITH: the names, and the word after "with" kept from every other
        // reading.
        var shielded = Set<Int>()
        var names: [String] = []
        var nameWords = Set<Int>()
        for i in low.indices where low[i] == "with" {
            shielded.insert(i + 1)
            let (found, used) = namesAfter(i, words: words, low: low)
            for name in found where names.count < maxNames { names.append(name) }
            shielded.formUnion(used)
            // The words that ARE names, not the "and" between them.
            nameWords.formUnion(used.filter { low[$0] != "and" && low[$0] != "&" })
        }
        var reading = TitleReading(title: raw.trimmingCharacters(in: .whitespacesAndNewlines), names: names)

        // RULE 1. A closing "with ..." that says nothing about when is set
        // aside, and the run is the words that end just before it.
        func runWord(_ k: Int) -> Bool { !shielded.contains(k) && low[k] != "with" && looksLikeWhen(low[k]) }
        var end = low.count
        if let with = low.lastIndex(of: "with"), with + 1 < low.count,
            !(with + 1..<low.count).contains(where: { runWord($0) && !["and", "or", "&"].contains(low[$0]) })
        {
            end = with
        }
        var from = end
        while from > 0, runWord(from - 1) { from -= 1 }
        // "dinner - friday": a dash before the run is the title's own.
        while from < end, low[from] == "-" || low[from] == "\u{2013}" { from += 1 }
        guard from < end else { return reading }
        // RULE 5.
        if from > 0, leadsNothing.contains(low[from - 1]) { return reading }

        // RULE 4: the run whole, or nothing.
        func word(_ k: Int) -> String? { k >= from && k < end ? low[k] : nil }
        var phrases: [Phrase] = []
        var at = from
        while at < end {
            guard
                let hit = timePhrase(at, word: word)
                    ?? dayPhrase(at, word: word, today: today, calendar: calendar)
            else { return reading }
            phrases.append(hit)
            at = hit.range.upperBound
        }
        // RULE 6.
        for (k, phrase) in phrases.enumerated() where !phrase.sure {
            let beside = (k > 0 && phrases[k - 1].sure) || (k + 1 < phrases.count && phrases[k + 1].sure)
            guard beside else { return reading }
        }
        let days = phrases.filter(\.isDay)
        let times = phrases.filter { !$0.isDay }
        guard days.allSatisfy({ $0.day == days[0].day }),
            times.allSatisfy({ $0.start == times[0].start && $0.end == times[0].end })
        else { return reading }

        // RULE 7: what is put in a well.
        if takeDay { reading.day = days.first?.day }
        if takeTime, let time = times.first { (reading.start, reading.end) = (time.start, time.end) }

        // RULE 3: led, the words that were read come out.
        guard removalLeads.contains(low[from]) || (low[from].hasPrefix("@") && low[from].count > 1) else { return reading }
        var taken = Array(repeating: false, count: words.count)
        for phrase in phrases where phrase.isDay ? takeDay : takeTime {
            for k in phrase.range { taken[k] = true }
        }
        guard taken.contains(true) else { return reading }
        let saved = rebuilt(words, taken: taken, nameWords: nameWords)
        if !saved.isEmpty { reading.title = saved }
        return reading
    }

    /// Leads that take their run out of the saved title with them.
    private static let removalLeads: Set<String> = ["on", "at", "@"]
    /// Words before a run that make it something other than when the event
    /// is: a subject, a deadline's far side, half a name.
    private static let leadsNothing: Set<String> = ["for", "before", "after", "of", "the"]
    /// Short forms that are also words: read only led, or beside a plain day
    /// or time.
    private static let wordlikeDays: Set<String> = ["sun", "sat", "wed"]
    private static let wordlikeMonths: Set<String> = ["mar", "may", "jun"]
    /// Words of a run that are not themselves a day, a month or a number.
    /// "until" and "till" only ever join two clocks, so a run they lead
    /// cannot be read (rule 5). "or" and "and" join nothing: they are here
    /// so that "dinner friday or saturday" is two days, and so nothing,
    /// instead of a Saturday with a Friday left unread before it.
    private static let runWords: Set<String> = [
        "on", "at", "@", "this", "next", "from", "to", "until", "till", "-", "\u{2013}", "or", "and", "&",
        "am", "pm", "a.m.", "p.m.", "noon", "midnight", "today", "tonight", "tomorrow", "tmrw",
    ]

    /// Whether a word could be part of a day or a time: the test for where
    /// the trailing run begins. Wide on purpose. A word that looks like one
    /// and cannot be read ("catch 22", "7-11", "call 1-800-555 4pm") makes
    /// the whole run unreadable, which is rule 4.
    private static func looksLikeWhen(_ w: String) -> Bool {
        if runWords.contains(w) || weekdayWord(w) != nil || monthWord(w) != nil || dayNumber(w) != nil { return true }
        let body = w.hasPrefix("@") ? String(w.dropFirst()) : w
        if body == "noon" || body == "midnight" { return w != body }
        guard let first = body.first, first.isNumber else { return false }
        return body.allSatisfy { $0.isNumber || ":.-\u{2013}/apm".contains($0) }
    }

    // MARK: words

    /// A word as it is matched: lowercased, the punctuation that ends a
    /// clause dropped from its end ("friday," is friday; "p.m." keeps its
    /// dots; "friday's" is not friday).
    static func normal(_ word: String) -> String {
        var w = word.lowercased()
        while let last = w.last, ",;!?".contains(last) { w.removeLast() }
        if w.hasSuffix("."), !w.hasSuffix(".m.") { w.removeLast() }
        return w
    }

    /// Words a name never is, and so where a run of names stops.
    private static let nameStops: Set<String> = [
        "on", "at", "@", "from", "in", "for", "about", "re", "this", "next", "to", "until", "till",
        "today", "tonight", "tomorrow", "tmrw", "noon", "midnight", "with", "the", "a", "an", "my", "our",
    ]

    // MARK: who

    /// The names after the "with" at `at`: "steven", "steven and ana",
    /// "steven, ana and bo", "steven ng". Each is one or two words of letters.
    /// The run stops at the first word that is not a name: a day, a time, a
    /// connector, a number. Returns the names and every word index they (and
    /// their "and"s) took.
    private static func namesAfter(_ at: Int, words: [String], low: [String]) -> ([String], Set<Int>) {
        var names: [String] = []
        var used = Set<Int>()
        var current: [String] = []
        func close() {
            if !current.isEmpty { names.append(current.joined(separator: " ")) }
            current = []
        }
        var j = at + 1
        while j < low.count {
            let w = low[j]
            if w == "and" || w == "&" {
                // Only between two names.
                guard !current.isEmpty, j + 1 < low.count, isNameWord(low[j + 1], next: j + 2 < low.count ? low[j + 2] : nil)
                else { break }
                close()
                used.insert(j)
                j += 1
                continue
            }
            guard isNameWord(w, next: j + 1 < low.count ? low[j + 1] : nil) else { break }
            if current.count == 2 { break }
            current.append(clean(words[j]))
            used.insert(j)
            // "steven," ends this name; the next word starts another.
            if words[j].hasSuffix(",") { close() }
            j += 1
        }
        close()
        return (names, used)
    }

    /// A name as shown: the typed word less the punctuation around it.
    private static func clean(_ word: String) -> String {
        word.trimmingCharacters(in: CharacterSet(charactersIn: ",;.!?"))
    }

    private static func isNameWord(_ w: String, next: String?) -> Bool {
        // Two LETTERS, as the daemon counts a name (`EventGuests.askable`):
        // "o'" and "j-" are one letter and a mark.
        guard w.filter({ $0.isLetter || $0.isNumber }).count >= 2, !nameStops.contains(w) else { return false }
        guard w.unicodeScalars.allSatisfy({ CharacterSet.letters.contains($0) || $0 == "-" || $0 == "'" })
        else { return false }
        // A day ends the names: "with steven friday", "with ana oct 12".
        // "with april" and "with may" are people until a day number follows.
        if weekdayWord(w) != nil { return false }
        if monthWord(w) != nil, let next, dayNumber(next) != nil { return false }
        return true
    }

    // MARK: when (days)

    private static let weekdayNames = [
        "sunday", "monday", "tuesday", "wednesday", "thursday", "friday", "saturday",
    ]
    private static let weekdayShort: [String: Int] = [
        "sun": 1, "mon": 2, "tue": 3, "tues": 3, "wed": 4, "weds": 4, "thu": 5, "thur": 5, "thurs": 5,
        "fri": 6, "sat": 7,
    ]
    private static let monthNames = [
        "january", "february", "march", "april", "may", "june", "july", "august", "september",
        "october", "november", "december",
    ]

    /// 1 (Sunday) ... 7 for a weekday's whole name or its usual short form,
    /// and whether it was the short form. Not any prefix: "wedding" and
    /// "sunny" are words.
    static func weekdayWord(_ w: String) -> (weekday: Int, short: Bool)? {
        if let i = weekdayNames.firstIndex(of: w) { return (i + 1, false) }
        if let n = weekdayShort[w] { return (n, true) }
        return nil
    }

    /// 1...12 for a month's whole name, its first three letters, or "sept".
    static func monthWord(_ w: String) -> Int? {
        if let i = monthNames.firstIndex(of: w) { return i + 1 }
        if w == "sept" { return 9 }
        if w.count == 3, let i = monthNames.firstIndex(where: { $0.hasPrefix(w) }) { return i + 1 }
        return nil
    }

    /// "8", "8th", "1st", "22nd", "3rd".
    static func dayNumber(_ w: String) -> Int? {
        var s = w
        for suffix in ["st", "nd", "rd", "th"] where s.hasSuffix(suffix) {
            s.removeLast(2)
            break
        }
        guard !s.isEmpty, s.count <= 2, s.allSatisfy(\.isNumber), let n = Int(s), (1...31).contains(n)
        else { return nil }
        return n
    }

    /// A day phrase at `p`: "friday", "on friday", "this fri", "on next tue",
    /// "today", "oct 12", "fri oct 9", "12 oct 2027", "on 10/12". Nil when the
    /// words there are not one, or are one that contradicts itself.
    private static func dayPhrase(
        _ p: Int, word: (Int) -> String?, today: Date, calendar: Calendar
    ) -> Phrase? {
        var k = p
        var led = false
        if word(k) == "on" {
            led = true
            k += 1
        }
        var which: String?
        if let w = word(k), w == "this" || w == "next" {
            which = w
            k += 1
        }
        guard let w = word(k) else { return nil }
        let today = calendar.startOfDay(for: today)
        func phrase(_ end: Int, _ day: Date, sure: Bool) -> Phrase {
            Phrase(range: p..<end, isDay: true, day: day, sure: led || sure)
        }
        if which == nil {
            switch w {
            case "today", "tonight": return phrase(k + 1, today, sure: true)
            case "tomorrow", "tmrw": return phrase(k + 1, EventForm.addDays(1, today, calendar), sure: true)
            default: break
            }
        }
        if let (weekday, _) = weekdayWord(w) {
            let plain = !wordlikeDays.contains(w)
            // "fri oct 9": the date decides, and the weekday must be its own.
            // "fri oct 10", a Saturday: not a day at all.
            if which == nil, let dated = monthDay(k + 1, word: word, today: today, calendar: calendar) {
                guard calendar.component(.weekday, from: dated.day) == weekday else { return nil }
                return phrase(dated.end, dated.day, sure: plain || dated.sure)
            }
            let ahead = (weekday - calendar.component(.weekday, from: today) + 7) % 7
            var day = EventForm.addDays(ahead, today, calendar)
            if which == "next" {
                // "next tue" is the Tuesday of NEXT week, by the user's own
                // weeks, never the one coming up in this one.
                let gone = (calendar.component(.weekday, from: today) - calendar.firstWeekday + 7) % 7
                let nextWeek = EventForm.addDays(7 - gone, today, calendar)
                let into = (weekday - calendar.firstWeekday + 7) % 7
                day = EventForm.addDays(into, nextWeek, calendar)
            }
            return phrase(k + 1, day, sure: plain || which != nil)
        }
        guard which == nil else { return nil }
        if let dated = monthDay(k, word: word, today: today, calendar: calendar) {
            return phrase(dated.end, dated.day, sure: dated.sure)
        }
        // "on 10/12": a slashed date only when "on" says so.
        let slashed = w.split(separator: "/", omittingEmptySubsequences: false)
        if led, slashed.count == 2 || slashed.count == 3,
            slashed.allSatisfy({ !$0.isEmpty && $0.count <= 4 && $0.allSatisfy(\.isNumber) }),
            let day = EventForm.parseDate(w, today: today, calendar: calendar),
            slashed.count == 2 || !yearGone(day, today: today, calendar: calendar)
        {
            return phrase(k + 1, day, sure: true)
        }
        return nil
    }

    /// A year was said and it is over.
    private static func yearGone(_ day: Date, today: Date, calendar: Calendar) -> Bool {
        calendar.component(.year, from: day) < calendar.component(.year, from: today)
    }

    /// "oct 12", "october 12th", "12 oct", each with an optional year after.
    /// Sure unless the month is one of the short forms that are also words
    /// and the day has no "th" on it ("may 5", "5 mar").
    private static func monthDay(
        _ i: Int, word: (Int) -> String?, today: Date, calendar: Calendar
    ) -> (end: Int, day: Date, sure: Bool)? {
        guard let a = word(i), let b = word(i + 1) else { return nil }
        let month: String
        let number: String
        if monthWord(a) != nil, dayNumber(b) != nil {
            (month, number) = (a, b)
        } else if dayNumber(a) != nil, monthWord(b) != nil {
            (month, number) = (b, a)
        } else {
            return nil
        }
        var text = "\(a) \(b)"
        var end = i + 2
        var year = false
        if let y = word(i + 2), y.count == 4, y.allSatisfy(\.isNumber), let n = Int(y), (2000...2999).contains(n) {
            text += " \(y)"
            end = i + 3
            year = true
        }
        guard let day = EventForm.parseDate(text, today: today, calendar: calendar) else { return nil }
        // "run oct 12 2000": a year already gone is not when anything is.
        if year, yearGone(day, today: today, calendar: calendar) { return nil }
        let sure = !wordlikeMonths.contains(month) || number.last?.isLetter == true
        return (end, day, sure)
    }

    // MARK: when (times)

    /// One typed clock: its text for `EventForm.parseTime`, and how sure the
    /// typing is.
    private struct Clock {
        let text: String
        /// "4pm", "4:30pm": am or pm said.
        let meridiem: Bool
        /// "16:30", "4:30": a colon.
        let colon: Bool
        /// "16:30", "08:00": a clock no bare count would be written as.
        var twentyFour: Bool {
            guard colon, let hour = Int(text.prefix { $0.isNumber }) else { return false }
            return hour > 12 || text.hasPrefix("0")
        }
        var explicit: Bool { meridiem || twentyFour }
    }

    /// "4", "4pm", "4:30", "4.30pm", "4:30p.m.": digits, an optional two
    /// digit minute, an optional am or pm. Nothing else ("4a", "1930", "q4",
    /// "5k").
    private static func clock(_ w: String) -> Clock? {
        var s = w.replacingOccurrences(of: "a.m.", with: "am").replacingOccurrences(of: "p.m.", with: "pm")
        var meridiem: String?
        for suffix in ["am", "pm"] where s.hasSuffix(suffix) {
            meridiem = suffix
            s.removeLast(2)
            break
        }
        let parts = s.split(separator: s.contains(":") ? ":" : ".", omittingEmptySubsequences: false).map(String.init)
        guard parts.count == 1 || parts.count == 2, let hour = parts.first, (1...2).contains(hour.count),
            hour.allSatisfy(\.isNumber)
        else { return nil }
        if parts.count == 2 {
            guard parts[1].count == 2, parts[1].allSatisfy(\.isNumber) else { return nil }
            // "4.30" with no am or pm is a number, not a clock.
            if !s.contains(":"), meridiem == nil { return nil }
        }
        let text = (parts.count == 2 ? "\(hour):\(parts[1])" : hour) + (meridiem ?? "")
        guard EventForm.parseTime(text) != nil else { return nil }
        return Clock(text: text, meridiem: meridiem != nil, colon: parts.count == 2)
    }

    /// A clock at word `k`, taking a separate "pm" after it ("4 pm"). Returns
    /// it and the index after it.
    private static func clockAt(_ k: Int, word: (Int) -> String?) -> (Clock, Int)? {
        guard let w = word(k), let c = clock(w) else { return nil }
        if !c.meridiem, let next = word(k + 1), ["am", "pm", "a.m.", "p.m."].contains(next),
            let joined = clock(c.text + next)
        {
            return (joined, k + 2)
        }
        return (c, k + 1)
    }

    /// A time phrase at `p` (its lead-in included), or nil: "4pm", "4 pm",
    /// "at 4:30", "@4pm", "noon", "4-6pm", "4pm to 6", "from 4 to 5:30".
    ///
    /// HOW SURE THE TYPING MUST BE. Am or pm said ("4pm", "4-6pm"), or a
    /// colon behind "at" or "from" ("at 4:30", "from 4 to 5:30"). A colon
    /// with no lead is a verse or a score ("Isaiah 14:12", "Romans
    /// 13:11-14"), and no am or pm and no colon is a number ("meet at 4",
    /// "from 3 to 4 people"): neither is a time.
    private static func timePhrase(_ p: Int, word: (Int) -> String?) -> Phrase? {
        guard var w = word(p) else { return nil }
        var k = p
        var lead: String?
        if w == "at" || w == "@" || w == "from" {
            lead = w == "from" ? "from" : "at"
            k = p + 1
            guard let next = word(k) else { return nil }
            w = next
        } else if w.hasPrefix("@"), w.count > 1 {
            // "@4pm", typed without the space.
            lead = "at"
            w = String(w.dropFirst())
        }
        let first = w
        func at(_ j: Int) -> String? { j == k ? first : word(j) }
        func phrase(_ end: Int, _ start: Int, _ stop: Int?) -> Phrase {
            Phrase(range: p..<end, isDay: false, day: nil, start: start, end: stop, sure: true)
        }
        func sure(_ a: Clock, _ b: Clock) -> Bool {
            a.meridiem || b.meridiem || (lead != nil && (a.colon || b.colon))
        }

        if w == "noon" || w == "midnight" {
            guard lead != "from" else { return nil }
            return phrase(k + 1, w == "noon" ? 720 : 0, nil)
        }

        // "4-6pm", "4pm-6pm", "at 4:30-5": one word, a dash between two clocks.
        for dash in ["-", "\u{2013}"] where w.contains(dash) {
            let sides = w.components(separatedBy: dash)
            guard sides.count == 2, let a = clock(sides[0]), let b = clock(sides[1]), sure(a, b),
                let span = resolve(a, b)
            else { return nil }
            return phrase(k + 1, span.start, span.end)
        }

        guard let (a, after) = clockAt(k, word: at) else { return nil }
        // "4 to 6pm", "4pm to 6", "from 4 to 5:30".
        if let joiner = word(after), ["to", "until", "till", "-", "\u{2013}"].contains(joiner) {
            guard let (b, end) = clockAt(after + 1, word: at), sure(a, b), let span = resolve(a, b)
            else { return nil }
            return phrase(end, span.start, span.end)
        }
        // One clock. "from 4pm" is a start with no end said.
        guard a.meridiem || (lead == "at" && a.colon) || (lead == "from" && a.twentyFour),
            let t = EventForm.parseTime(a.text)
        else { return nil }
        return phrase(after, t.minutes, nil)
    }

    /// Two clocks as a span. A side with no am or pm takes it from the
    /// other: "4-6pm" is 4pm, "11-1pm" is 11am (the latest reading that
    /// still comes before the end), "4pm to 6" is 6pm. Neither said ("from 4
    /// to 5:30"): the form's own reading of a bare hour.
    ///
    /// IT MUST END AFTER IT STARTS, on the same day, or run from an evening
    /// into the next morning ("10pm-2am"). "7pm-6pm" would be 23 hours and
    /// "4-4pm" twelve: nobody means those, so they are not spans.
    private static func resolve(_ a: Clock, _ b: Clock) -> (start: Int, end: Int)? {
        guard let ta = EventForm.parseTime(a.text) else { return nil }
        var start = ta.minutes
        let end: Int
        if a.explicit || !b.explicit {
            guard let stop = EventForm.parseEnd(b.text, after: start) else { return nil }
            end = stop
        } else {
            guard let tb = EventForm.parseTime(b.text) else { return nil }
            end = tb.minutes
            let hour = start % 720
            // The readings of the bare start, the one nearest before the end
            // first; none before it ("10-2am") is the evening before.
            start = [hour + 720, hour].first { $0 < end } ?? hour + 720
            guard start % 720 != end % 720 else { return nil }
        }
        guard end > start || (start >= 720 && end < 720) else { return nil }
        return (start, end)
    }

    // MARK: the saved title

    /// The words left, in order, tidied: a clause mark left hanging where
    /// words came out is dropped, and a first letter is given a capital only
    /// when the words that are kept hold none.
    ///
    /// A NAME GETS ITS CAPITAL. Each word of a "with <name>" typed all in
    /// lower case is given one ("with steven and ana" saves as "with Steven
    /// and Ana"); a name the user capitalised or mixed ("McKay", "DJ", "van")
    /// is theirs and is left exactly as typed, and "and" is not a name.
    private static func rebuilt(_ words: [String], taken: [Bool], nameWords: Set<Int>) -> String {
        var kept: [String] = []
        // The capital is decided by the words that STAY: "dinner on Friday"
        // keeps "dinner", which has none, so it gets one.
        var typedCapital = false
        for (k, word) in words.enumerated() where !taken[k] {
            if word.contains(where: \.isUppercase) { typedCapital = true }
            kept.append(nameWords.contains(k) ? named(word) : word)
        }
        // "dinner, friday 7pm" leaves "dinner,": the comma was about what
        // went.
        if let lastKept = words.indices.last(where: { !taken[$0] }), lastKept < words.count - 1, var last = kept.popLast() {
            while let c = last.last, ",;:-".contains(c) { last.removeLast() }
            if !last.isEmpty { kept.append(last) }
        }
        while let last = kept.last, ["-", "\u{2013}", ",", ":"].contains(last) { kept.removeLast() }
        while let first = kept.first, ["-", "\u{2013}", ",", ":"].contains(first) { kept.removeFirst() }
        var title = kept.joined(separator: " ")
        if !typedCapital, let first = title.first {
            title = first.uppercased() + title.dropFirst()
        }
        return title
    }
}

extension TitleParse {
    /// A name word as saved: all lower case as typed, each part of it
    /// ("mary-jane") takes a capital; anything else is left alone.
    static func named(_ word: String) -> String {
        guard !word.contains(where: \.isUppercase) else { return word }
        return word.split(separator: "-", omittingEmptySubsequences: false).map { part in
            part.first.map { $0.uppercased() + part.dropFirst() } ?? ""
        }.joined(separator: "-")
    }
}

// MARK: - the title in the form

/// The when fields a title can fill.
enum TitleField: String, Hashable, Sendable, CaseIterable {
    case date
    case start
    case end
}

/// How a draft's title is being read, kept on the draft (so the grid's chip,
/// the save and a parked pane all see the same thing).
struct TitleRead: Hashable, Sendable {
    /// A NEW event's title is read. An edit's never is: its words were saved
    /// as they are.
    var on = false
    /// Esc on the title, or one undo: this title is taken as typed. Cleared
    /// when the title is emptied.
    var literal = false
    /// What each field the title filled held before, so deleting the words
    /// puts it back.
    var filled: [TitleField: String] = [:]
    /// Fields the user set by hand: the title never writes them again.
    var hand: Set<TitleField> = []
    /// Names whose suggestion was answered (added or dismissed), folded to
    /// lower case: it does not come back for this pane.
    var answered: Set<String> = []

    var reads: Bool { on && !literal }
}

extension EventForm {
    /// What the draft's title says, as the pane will act on it: nil when the
    /// title is not being read. A field the user set by hand is not the
    /// title's to fill, and its words are not taken out of the title either:
    /// only words whose reading is put in a well ever come out.
    static func titleReading(_ d: EventDraft, today: Date, calendar: Calendar) -> TitleReading? {
        guard d.titleRead.reads else { return nil }
        let hand = d.titleRead.hand
        return TitleParse.read(
            d.title, today: today, calendar: calendar,
            takeDay: !hand.contains(.date), takeTime: !d.allDay && !hand.contains(.start))
    }

    /// The saved title is not the typed one: words came out of it. This, and
    /// not a filled well, is what puts the "saves as" line under the title;
    /// a title that fills the wells and keeps its words shows on the wells.
    static func titleSavesChanged(_ d: EventDraft, today: Date, calendar: Calendar) -> Bool {
        savedTitle(d, today: today, calendar: calendar) != trimmed(d.title)
    }

    /// What a reading puts in the wells.
    private static func texts(_ reading: TitleReading?, today: Date, calendar: Calendar) -> [TitleField: String] {
        var out: [TitleField: String] = [:]
        if let day = reading?.day { out[.date] = dateText(day, today: today, calendar: calendar) }
        if let start = reading?.start {
            out[.start] = clockText(start)
            // No end said: an hour, as a new event's is.
            out[.end] = clockText((reading?.end ?? start + 60) % 1440)
        }
        return out
    }

    /// The title that is saved, and drawn on the grid.
    static func savedTitle(_ d: EventDraft, today: Date, calendar: Calendar) -> String {
        titleReading(d, today: today, calendar: calendar)?.title ?? trimmed(d.title)
    }

    /// The title typed: the draft with it, and its when fields filled (or put
    /// back) by what the title now says.
    static func retitled(_ d: EventDraft, title: String, today: Date, calendar: Calendar) -> EventDraft {
        var out = d
        out.title = title
        // An emptied title starts over: whatever was said about taking it
        // literally was about the words that are gone.
        if trimmed(title).isEmpty { out.titleRead.literal = false }
        return refilled(out, today: today, calendar: calendar)
    }

    /// The when fields brought in line with the title as it stands. Each
    /// field the title names is filled (its old text kept for the way back);
    /// each it no longer names gets its old text back. Hand-set fields are
    /// never touched.
    static func refilled(_ d: EventDraft, today: Date, calendar: Calendar) -> EventDraft {
        var out = d
        let reading = titleReading(d, today: today, calendar: calendar)
        let wanted = texts(reading, today: today, calendar: calendar)
        for field in TitleField.allCases where !out.titleRead.hand.contains(field) {
            let path = keyPath(field)
            if let text = wanted[field] {
                if out.titleRead.filled[field] == nil { out.titleRead.filled[field] = out[keyPath: path] }
                out[keyPath: path] = text
            } else if let before = out.titleRead.filled[field] {
                out[keyPath: path] = before
                out.titleRead.filled[field] = nil
            }
        }
        return out
    }

    /// A when field typed into, or a day clicked on the grid: the field is
    /// the user's from here, and the title stops writing it.
    ///
    /// A START TAKES ITS END WITH IT. A start typed over the title's "7pm"
    /// takes the end as it stands: otherwise the end would fall back to what
    /// it was before the title, and an 8pm start would be left ending at an
    /// hour the user never saw beside it.
    ///
    /// AN END CORRECTED LEAVES THE TITLE ITS START. "dinner on friday at 7pm",
    /// then 9:30pm typed into the end: the start is still the title's, so its
    /// words stay out of the saved title and a later "at 8pm" still moves it.
    /// An end typed beside a start the title did NOT write takes the start
    /// too: the user has set a span, and a time typed later in the title
    /// must not slide one end of it.
    static func handSet(_ d: EventDraft, _ field: TitleField) -> EventDraft {
        var out = d
        let fields: [TitleField]
        switch field {
        case .date: fields = [.date]
        case .start: fields = [.start, .end]
        case .end: fields = out.titleRead.filled[.start] != nil ? [.end] : [.start, .end]
        }
        for f in fields {
            out.titleRead.hand.insert(f)
            out.titleRead.filled[f] = nil
        }
        return out
    }

    /// Esc on the title, or one undo: the title is taken exactly as typed,
    /// and what it had filled goes back to what was there.
    static func literalTitle(_ d: EventDraft, today: Date, calendar: Calendar) -> EventDraft {
        var out = d
        out.titleRead.literal = true
        return refilled(out, today: today, calendar: calendar)
    }

    /// Whether Esc on the title has anything to undo: the title is being
    /// read and it has filled a field (a changed saved title always has).
    static func titleIsRead(_ d: EventDraft, today: Date, calendar: Calendar) -> Bool {
        titleReading(d, today: today, calendar: calendar)?.filled == true
    }

    private static func keyPath(_ field: TitleField) -> WritableKeyPath<EventDraft, String> {
        switch field {
        case .date: \.date
        case .start: \.start
        case .end: \.end
        }
    }
}
