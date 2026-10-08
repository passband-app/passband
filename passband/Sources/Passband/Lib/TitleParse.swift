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
// WHAT IT READS.
//   - A day: "on friday", "fri", "this fri", "next tue", "today", "tonight",
//     "tomorrow", "oct 12", "october 12th", "12 oct", "on 10/12".
//   - A time or a range: "4pm", "4 pm", "at 4:30", "at noon", "16:30",
//     "4-6pm", "4pm to 6pm", "from 4 to 5:30".
//   - Who: "with steven", "with steven and ana", "with steven, ana and bo".
//
// AMBIGUITY RESOLVES TO NOTHING. It is a convenience on a field whose text is
// saved on somebody's calendar, so every rule errs toward leaving the title
// alone:
//   - A bare number is never a time. "meet at 4" could be a room, a count or
//     an address; only a meridiem ("4pm"), a 24-hour clock ("16:30"), a colon
//     after "at" ("at 4:30"), or a stated range ("from 4 to 5:30", "4-6pm")
//     is one.
//   - The word after "with" is never a day or a time: "call with 4pm people".
//   - A weekday must be the word itself ("lunch at Friday's" is a place), and
//     a three-letter one ("sun", "sat", "mon") needs something beside it that
//     says it is a day: "on", "this", "next", a time, or the end of the title.
//   - A month needs its day number ("May day parade" has none).
//   - Two different days, or two different times, are a title about
//     something else: neither is taken.
//
// WHAT IS SAVED. The words that named the day and the time come out of the
// title ("on friday 4pm"), with the "on" / "at" / "from" that led them. The
// "with <name>" words stay, and a name typed in lower case gets its capital:
// "Dinner with Steven". When the user typed no capital at all, the first
// letter gets one. A title with nothing recognised
// is returned exactly as typed.

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

    /// Read a title. `takeDay` / `takeTime` false leaves that kind of word in
    /// the title and out of the reading: the pane passes false for a field
    /// the user has set by hand, so words that would not fill anything are
    /// not taken out of the title either.
    static func read(
        _ raw: String, today: Date, calendar: Calendar, takeDay: Bool = true, takeTime: Bool = true
    ) -> TitleReading {
        let words = raw.split(whereSeparator: \.isWhitespace).map(String.init)
        let low = words.map(normal)
        var taken = Array(repeating: false, count: words.count)

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

        // TIMES first: a three-letter weekday may lean on one.
        var times: [(range: Range<Int>, start: Int, end: Int?)] = []
        var i = 0
        while i < low.count {
            if let hit = timeAt(i, low: low, shielded: shielded) {
                times.append(hit)
                i = hit.range.upperBound
            } else {
                i += 1
            }
        }
        let timeWords = Set(times.flatMap { Array($0.range) })

        var days: [(range: Range<Int>, day: Date)] = []
        i = 0
        while i < low.count {
            if !timeWords.contains(i),
                let hit = dayAt(
                    i, low: low, shielded: shielded, timeWords: timeWords, today: today, calendar: calendar)
            {
                days.append(hit)
                i = hit.range.upperBound
            } else {
                i += 1
            }
        }

        var reading = TitleReading(title: raw.trimmingCharacters(in: .whitespacesAndNewlines), names: names)
        // One day, said once (or the same day said twice): taken. Two
        // different days: neither.
        if takeDay, let first = days.first, days.allSatisfy({ $0.day == first.day }) {
            reading.day = first.day
            for hit in days { for k in hit.range { taken[k] = true } }
        }
        if takeTime, times.count == 1, let only = times.first {
            (reading.start, reading.end) = (only.start, only.end)
            for k in only.range { taken[k] = true }
        }
        guard reading.filled else { return reading }
        reading.title = rebuilt(words, taken: taken, nameWords: nameWords, raw: raw)
        return reading
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

    private static let leads: Set<String> = ["on", "at", "@", "from", "this", "next"]
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
        guard w.count >= 2, !nameStops.contains(w) else { return false }
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

    /// A day phrase starting at `i` (its lead-in word included), or nil.
    private static func dayAt(
        _ i: Int, low: [String], shielded: Set<Int>, timeWords: Set<Int>, today: Date, calendar: Calendar
    ) -> (range: Range<Int>, day: Date)? {
        guard !shielded.contains(i) else { return nil }
        func word(_ k: Int) -> String? {
            k >= 0 && k < low.count && !shielded.contains(k) && !timeWords.contains(k) ? low[k] : nil
        }
        // "on <day>", "this <weekday>", "next <weekday>": the lead comes out
        // of the title with the day, and only with one.
        if low[i] == "on" || low[i] == "this" || low[i] == "next" {
            guard let hit = bareDay(i + 1, word: word, lead: low[i], low: low, timeWords: timeWords, today: today, calendar: calendar)
            else { return nil }
            return (i..<hit.range.upperBound, hit.day)
        }
        return bareDay(i, word: word, lead: nil, low: low, timeWords: timeWords, today: today, calendar: calendar)
    }

    private static func bareDay(
        _ i: Int, word: (Int) -> String?, lead: String?, low: [String], timeWords: Set<Int>, today: Date,
        calendar: Calendar
    ) -> (range: Range<Int>, day: Date)? {
        guard let w = word(i) else { return nil }
        let today = calendar.startOfDay(for: today)
        if lead == nil || lead == "on" {
            switch w {
            case "today", "tonight": return (i..<i + 1, today)
            case "tomorrow", "tmrw": return (i..<i + 1, EventForm.addDays(1, today, calendar))
            default: break
            }
        }
        if let (weekday, short) = weekdayWord(w) {
            // "fri oct 9": the date decides, and the weekday goes with it.
            if lead != "next", lead != "this", let dated = monthDay(i + 1, word: word, today: today, calendar: calendar) {
                return (i..<dated.range.upperBound, dated.day)
            }
            // A three-letter weekday needs a neighbour that says it is a day:
            // its lead-in, a time on either side, or the end of the title.
            if short, lead == nil, i + 1 < low.count, !timeWords.contains(i + 1), !timeWords.contains(i - 1) {
                return nil
            }
            let ahead = (weekday - calendar.component(.weekday, from: today) + 7) % 7
            var day = EventForm.addDays(ahead, today, calendar)
            if lead == "next" {
                // "next tue" is the Tuesday of NEXT week, by the user's own
                // weeks, never the one coming up in this one.
                let gone = (calendar.component(.weekday, from: today) - calendar.firstWeekday + 7) % 7
                let nextWeek = EventForm.addDays(7 - gone, today, calendar)
                let into = (weekday - calendar.firstWeekday + 7) % 7
                day = EventForm.addDays(into, nextWeek, calendar)
            }
            return (i..<i + 1, day)
        }
        guard lead != "this", lead != "next" else { return nil }
        if let dated = monthDay(i, word: word, today: today, calendar: calendar) { return dated }
        // "on 10/12": a slashed date only when "on" says so.
        let slashed = w.split(separator: "/", omittingEmptySubsequences: false)
        if lead == "on", slashed.count == 2 || slashed.count == 3,
            slashed.allSatisfy({ !$0.isEmpty && $0.count <= 4 && $0.allSatisfy(\.isNumber) }),
            let day = EventForm.parseDate(w, today: today, calendar: calendar)
        {
            return (i..<i + 1, day)
        }
        return nil
    }

    /// "oct 12", "october 12th", "12 oct", each with an optional year after.
    private static func monthDay(
        _ i: Int, word: (Int) -> String?, today: Date, calendar: Calendar
    ) -> (range: Range<Int>, day: Date)? {
        guard let a = word(i), let b = word(i + 1) else { return nil }
        let isPair = (monthWord(a) != nil && dayNumber(b) != nil) || (dayNumber(a) != nil && monthWord(b) != nil)
        guard isPair else { return nil }
        var text = "\(a) \(b)"
        var end = i + 2
        if let y = word(i + 2), y.count == 4, y.allSatisfy(\.isNumber), let n = Int(y), (2000...2999).contains(n) {
            text += " \(y)"
            end = i + 3
        }
        guard let day = EventForm.parseDate(text, today: today, calendar: calendar) else { return nil }
        return (i..<end, day)
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

    /// A time phrase starting at `i` (its lead-in word included), or nil.
    private static func timeAt(
        _ i: Int, low: [String], shielded: Set<Int>
    ) -> (range: Range<Int>, start: Int, end: Int?)? {
        guard !shielded.contains(i) else { return nil }
        func word(_ k: Int) -> String? { k >= 0 && k < low.count && !shielded.contains(k) ? low[k] : nil }
        var lead: String?
        var k = i
        if ["at", "@", "from"].contains(low[i]) {
            lead = low[i] == "@" ? "at" : low[i]
            k = i + 1
        }
        guard let w = word(k) else { return nil }

        // "noon", "midnight": after "at", or as the title's last word. "High
        // noon screening" and "Midnight Mass" are titles.
        if w == "noon" || w == "midnight" {
            guard lead == "at" || (lead == nil && k == low.count - 1) else { return nil }
            return (i..<k + 1, w == "noon" ? 720 : 0, nil)
        }

        // "4-6pm", "4pm-6pm", "4:30-5": one word, a dash between two clocks.
        for dash in ["-", "\u{2013}"] where w.contains(dash) {
            let sides = w.components(separatedBy: dash)
            guard sides.count == 2, let a = clock(sides[0]), let b = clock(sides[1]),
                a.explicit || b.explicit || lead == "from",
                let span = resolve(a, b)
            else { return nil }
            return (i..<k + 1, span.start, span.end)
        }

        guard let (a, after) = clockAt(k, word: word) else { return nil }
        // "4 to 6pm", "4pm to 6", "from 4 to 5:30".
        if let joiner = word(after), ["to", "until", "till", "-", "\u{2013}"].contains(joiner),
            let (b, end) = clockAt(after + 1, word: word),
            a.explicit || b.explicit || lead == "from", let span = resolve(a, b)
        {
            return (i..<end, span.start, span.end)
        }
        // One clock. "from 4pm" is a start with no end said; "from" alone
        // with a bare number is nothing.
        guard a.explicit || (lead == "at" && a.colon), let t = EventForm.parseTime(a.text) else { return nil }
        return (i..<after, t.minutes, nil)
    }

    /// Two clocks as a span. A side with no am or pm takes it from the
    /// other: "4-6pm" is 4pm, "11-1pm" is 11am (the latest reading that
    /// still comes before the end), "4pm to 6" is 6pm. Neither said ("from 4
    /// to 5:30"): the form's own reading of a bare hour.
    private static func resolve(_ a: Clock, _ b: Clock) -> (start: Int, end: Int)? {
        guard let ta = EventForm.parseTime(a.text) else { return nil }
        if a.explicit || !b.explicit {
            guard let end = EventForm.parseEnd(b.text, after: ta.minutes), end != ta.minutes else { return nil }
            return (ta.minutes, end)
        }
        guard let tb = EventForm.parseTime(b.text) else { return nil }
        let end = tb.minutes
        let hour = ta.minutes % 720
        // The readings of the bare start, the one nearest before the end
        // first; none before it ("10-2am") is the evening before.
        let start = [hour + 720, hour].first { $0 < end } ?? hour + 720
        guard start != end else { return nil }
        return (start, end)
    }

    // MARK: the saved title

    /// The words left, in order, tidied: a clause mark left hanging where
    /// words came out is dropped, and a first letter is given a capital only
    /// when the user typed none anywhere.
    ///
    /// A NAME GETS ITS CAPITAL. Each word of a "with <name>" typed all in
    /// lower case is given one ("with steven and ana" saves as "with Steven
    /// and Ana"); a name the user capitalised or mixed ("McKay", "DJ", "van")
    /// is theirs and is left exactly as typed, and "and" is not a name.
    private static func rebuilt(_ words: [String], taken: [Bool], nameWords: Set<Int>, raw: String) -> String {
        var kept: [String] = []
        for (k, word) in words.enumerated() where !taken[k] {
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
        if !raw.contains(where: \.isUppercase), let first = title.first {
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
    /// title is not being read. Words for a hand-set field stay in the title.
    static func titleReading(_ d: EventDraft, today: Date, calendar: Calendar) -> TitleReading? {
        guard d.titleRead.reads else { return nil }
        let hand = d.titleRead.hand
        return TitleParse.read(
            d.title, today: today, calendar: calendar, takeDay: !hand.contains(.date),
            takeTime: !d.allDay && !hand.contains(.start) && !hand.contains(.end))
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
        var wanted: [TitleField: String] = [:]
        if let day = reading?.day { wanted[.date] = dateText(day, today: today, calendar: calendar) }
        if let start = reading?.start {
            wanted[.start] = clockText(start)
            // No end said: an hour, as a new event's is.
            wanted[.end] = clockText((reading?.end ?? start + 60) % 1440)
        }
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
    /// THE TWO TIMES GO TOGETHER. A start typed over the title's "7pm" takes
    /// the end with it, as it stands: otherwise the end would fall back to
    /// what it was before the title, and an 8pm start would be left ending
    /// at an hour the user never saw beside it.
    static func handSet(_ d: EventDraft, _ field: TitleField) -> EventDraft {
        var out = d
        for f in field == .date ? [TitleField.date] : [.start, .end] {
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
    /// read and it has changed the saved title or filled a field.
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
