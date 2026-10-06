// THE EVENT EDITOR'S ARITHMETIC: what the user typed into the card, whether
// it can be saved, what it says it will save, and the exact body the daemon
// is sent (`EventWrite`, `EventPatch`: Lib/CalendarWrite.swift, beside the
// rest of what surrounds a write).
//
// PURE, and Foundation only, so test.sh holds it against fixtures
// (Tests/EventDraftTests.swift). Clock and zone are arguments.
//
// TYPED, NOT PICKED. The date and the times are text fields that read what
// people type ("Oct 8", "fri", "10/8", "7:45pm", "1930", "noon"), and the
// card states, under the fields, exactly what it will save ("Thu Oct 8, 7:45
// to 8:45pm"): the remind palette's rule that every typed time says the
// instant it resolves to. A bare hour reads the way people mean it: a start
// of "7" is 7am and "3" is 3pm, and an end of "9" after a 7pm start is 9pm.
// An end time that is not after the start is the next day, so a 10pm to 2am
// night is typed as it is said.
//
// THE WIRE (docs: wave 3 contract): timed `start` / `end` are RFC3339 with the
// offset of that instant in the user's zone; all-day are `YYYY-MM-DD` with the
// END THE LAST DAY, INCLUSIVE, the convention the timeline serves (the daemon
// converts to and from Google's exclusive end).

import Foundation

// MARK: - the form

/// The editor's fields, as typed.
struct EventDraft: Hashable, Sendable {
    var title = ""
    var calendarId: String?
    var allDay = false
    /// The day, or the first day of an all-day run.
    var date = ""
    /// All-day: the last day, inclusive ("" is the same day). Timed: the day
    /// it ends, ONLY for an event that runs past the next morning ("" is the
    /// start's day, or the next when the end time is not after the start).
    var endDate = ""
    var start = ""
    var end = ""
    var location = ""
    var notes = ""
    /// Wave 5. The guests line, as the recipient field holds it: addresses
    /// joined by commas, a half-typed one last (read by `EventGuests.read`).
    var guests = ""
}

/// Why a draft cannot be saved yet, in the card's own words. One at a time,
/// in the order the fields are drawn.
enum EventDraftProblem: Error, Equatable, Sendable {
    case noTitle
    case titleTooLong
    case noCalendar
    case badDate
    case badEndDate
    case badStart
    case badEnd
    case noLength
    case endsBeforeStart
    case notesTooLong
    case badGuest
    case guestIsYou
    case tooManyGuests

    var text: String {
        switch self {
        case .noTitle: "Needs a title"
        case .titleTooLong: "Titles stop at 200 characters"
        case .noCalendar: "Pick a calendar"
        case .badDate: "Try a date like Oct 8 or 10/8"
        case .badEndDate: "Try an end date like Oct 9"
        case .badStart: "Try a start like 7pm or 19:30"
        case .badEnd: "Try an end like 8pm or 20:30"
        case .noLength: "It ends when it starts"
        case .endsBeforeStart: "It ends before it starts"
        case .notesTooLong: "Notes stop at 2,000 characters"
        case .badGuest: "A guest needs a whole email address"
        case .guestIsYou: "You are on it already. Take your own address off"
        case .tooManyGuests: "An event takes 50 guests at most"
        }
    }
}

/// When a draft is, resolved. Timed: two instants, end exclusive. All-day:
/// two local midnights, `end` the LAST day (inclusive).
struct ResolvedWhen: Hashable, Sendable {
    let allDay: Bool
    let start: Date
    let end: Date
}

/// A time of day, typed: minutes after midnight, and whether the typing
/// settled am or pm itself ("7pm", "19:00", "noon") or left it to us ("7").
struct TimeOfDay: Hashable, Sendable {
    let minutes: Int
    let explicit: Bool
}

// MARK: - reading, checking, writing

enum EventForm {
    /// The daemon's title cap (contract: 1 to 200 characters).
    static let maxTitle = 200
    /// The served description's cap; a longer note would come back cut.
    static let maxNotes = 2000

    /// Characters as the daemon counts them (Rust's `chars()`: scalars).
    static func length(_ s: String) -> Int { s.unicodeScalars.count }

    static func trimmed(_ s: String) -> String { s.trimmingCharacters(in: .whitespacesAndNewlines) }

    // MARK: drafts to start from

    /// A new event on `day`: today starts at the next whole hour, any other
    /// day at 9am, an hour long. The calendar is the default writable one.
    static func newDraft(day: Date, now: Date, calendarId: String?, calendar: Calendar) -> EventDraft {
        let day = calendar.startOfDay(for: day)
        var startMinutes = 9 * 60
        if calendar.isDate(day, inSameDayAs: now) {
            let h = calendar.component(.hour, from: now)
            startMinutes = min(23, h + 1) * 60
        }
        var d = EventDraft()
        d.calendarId = calendarId
        d.date = dateText(day, today: now, calendar: calendar)
        d.start = clockText(startMinutes)
        d.end = clockText((startMinutes + 60) % 1440)
        return d
    }

    /// A new event from a span swept on the day column: that day, those
    /// minutes after midnight. An end at 1440 is midnight, which the form
    /// reads as the day's end ("11pm to 12am").
    static func newDraft(
        day: Date, startMinutes: Int, endMinutes: Int, now: Date, calendarId: String?, calendar: Calendar
    ) -> EventDraft {
        var d = EventDraft()
        d.calendarId = calendarId
        d.date = dateText(calendar.startOfDay(for: day), today: now, calendar: calendar)
        d.start = clockText(max(0, min(1439, startMinutes)))
        d.end = clockText(max(0, endMinutes) % 1440)
        return d
    }

    /// The draft moved to another day (a day clicked on the grid beside the
    /// pane). A run of days keeps its length: the end day moves with the
    /// start. An end day that does not read is left for the form to say so.
    static func moved(_ d: EventDraft, to day: Date, today: Date, calendar: Calendar) -> EventDraft {
        let day = calendar.startOfDay(for: day)
        var out = d
        out.date = dateText(day, today: today, calendar: calendar)
        let endText = trimmed(d.endDate)
        if !endText.isEmpty, let from = parseDate(d.date, today: today, calendar: calendar),
            let last = parseDate(endText, today: today, calendar: calendar), last >= from
        {
            let length = calendar.dateComponents([.day], from: from, to: last).day ?? 0
            out.endDate = dateText(addDays(length, day, calendar), today: today, calendar: calendar)
        }
        return out
    }

    // MARK: the draft on the grid

    /// The row id the draft is drawn under while the pane is open. Not an
    /// event's id (`Agenda.itemId` always has a calendar and a bar in it).
    static let provisionalId = "draft"

    /// The draft as a row for the month: where it would land, in its
    /// calendar's colour, so the grid shows it move as the fields change.
    /// Nil while the times do not read. `color` is the chosen calendar's (or,
    /// for an edit, the event's own).
    static func provisional(
        _ d: EventDraft, color: String?, today: Date, calendar: Calendar
    ) -> AgendaItem? {
        guard case .success(let w) = when(d, today: today, calendar: calendar) else { return nil }
        let title = trimmed(d.title)
        let (start, end) = wireTimes(w, calendar)
        let event = TimelineEvent(
            id: provisionalId, calendar_id: d.calendarId ?? "", calendar_name: nil,
            title: title.isEmpty ? "New event" : title, start: start, end: end, all_day: w.allDay,
            start_timezone: nil, location: nil, status: .tentative, self_response: nil, busy: true,
            organizer: nil, html_link: nil, color: color, editable: false, description: nil)
        guard let item = Agenda.item(event, calendar: calendar) else { return nil }
        return AgendaItem(
            id: provisionalId, origin: item.origin, title: item.title, location: nil, start: item.start,
            end: item.end, allDay: item.allDay, endEstimated: false, fill: .outline, band: nil,
            cancelled: false, unverified: false, folded: false)
    }

    /// The rows the month draws while the pane is open: the feed's own, less
    /// the event being edited (the draft stands in for it), plus the draft.
    static func withProvisional(
        _ items: [AgendaItem], _ provisional: AgendaItem?, editing: TimelineEvent?
    ) -> [AgendaItem] {
        guard let provisional else { return items }
        let hidden = editing.map(Agenda.itemId)
        return items.filter { $0.id != hidden && $0.id != provisionalId } + [provisional]
    }

    /// The card filled in from an event on screen. Prints only what the
    /// parsers read back, so an untouched field saves what was there.
    static func draft(editing event: TimelineEvent, today: Date, calendar: Calendar) -> EventDraft {
        var d = EventDraft()
        d.title = event.title ?? ""
        d.calendarId = event.calendar_id
        d.location = event.location ?? ""
        d.notes = event.description ?? ""
        d.guests = EventGuests.fieldText(event)
        guard let item = Agenda.item(event, calendar: calendar) else { return d }
        d.allDay = item.allDay
        d.date = dateText(item.start, today: today, calendar: calendar)
        if item.allDay {
            // The item's end is exclusive (the midnight after); the form
            // names the last day.
            let last = calendar.date(byAdding: .day, value: -1, to: item.effectiveEnd) ?? item.start
            d.endDate = dateText(max(item.start, calendar.startOfDay(for: last)), today: today, calendar: calendar)
            return d
        }
        let end = item.effectiveEnd
        d.start = Agenda.clock(item.start, calendar)
        d.end = Agenda.clock(end, calendar)
        // The end day is said only when the rule cannot find it: the same
        // day, or the next one with an end time not after the start.
        let startDay = calendar.startOfDay(for: item.start)
        let endDay = calendar.startOfDay(for: end)
        let next = calendar.date(byAdding: .day, value: 1, to: startDay) ?? startDay
        let s = minutesOfDay(item.start, calendar)
        let e = minutesOfDay(end, calendar)
        let ruleFinds = endDay == startDay ? e > s : (endDay == next && e < s)
        if !ruleFinds { d.endDate = dateText(end, today: today, calendar: calendar) }
        return d
    }

    // MARK: checking

    /// When the draft is, or why it cannot be read.
    static func when(_ d: EventDraft, today: Date, calendar: Calendar) -> Result<ResolvedWhen, EventDraftProblem> {
        guard let day = parseDate(d.date, today: today, calendar: calendar) else { return .failure(.badDate) }
        let endDateText = trimmed(d.endDate)
        if d.allDay {
            var last = day
            if !endDateText.isEmpty {
                guard let parsed = parseDate(endDateText, today: today, calendar: calendar) else {
                    return .failure(.badEndDate)
                }
                last = parsed
            }
            guard last >= day else { return .failure(.endsBeforeStart) }
            return .success(ResolvedWhen(allDay: true, start: day, end: last))
        }
        guard let s = parseTime(d.start) else { return .failure(.badStart) }
        guard let e = parseEnd(d.end, after: s.minutes) else { return .failure(.badEnd) }
        guard let start = at(day, s.minutes, calendar) else { return .failure(.badDate) }
        var endDay = day
        if !endDateText.isEmpty {
            guard let parsed = parseDate(endDateText, today: today, calendar: calendar) else {
                return .failure(.badEndDate)
            }
            endDay = parsed
        }
        guard var end = at(endDay, e, calendar) else { return .failure(.badEndDate) }
        if endDateText.isEmpty {
            if e == s.minutes { return .failure(.noLength) }
            // Not after the start: the next day. "10pm to 2am".
            if end <= start, let next = at(addDays(1, day, calendar), e, calendar) { end = next }
        }
        guard end > start else { return .failure(.endsBeforeStart) }
        return .success(ResolvedWhen(allDay: false, start: start, end: end))
    }

    /// The first thing in the way of saving a NEW event, in field order.
    static func problem(
        _ d: EventDraft, today: Date, calendar: Calendar, guests: GuestTerms = .closed
    ) -> EventDraftProblem? {
        if case .failure(let p) = wire(d, today: today, calendar: calendar, guests: guests) { return p }
        return nil
    }

    private static func titleProblem(_ title: String) -> EventDraftProblem? {
        if title.isEmpty { return .noTitle }
        if length(title) > maxTitle { return .titleTooLong }
        return nil
    }

    // MARK: writing

    /// A new event's body. `guests` says whether the guests line is open for
    /// this draft; closed (the default), the body names nobody whatever the
    /// line holds. `notify` is never set here: `EventGuests.notify` decides
    /// it, at the moment a labelled control is pressed.
    static func wire(
        _ d: EventDraft, today: Date, calendar: Calendar, guests: GuestTerms = .closed
    ) -> Result<EventWrite, EventDraftProblem> {
        let title = trimmed(d.title)
        if let p = titleProblem(title) { return .failure(p) }
        guard let calendarId = d.calendarId, !calendarId.isEmpty else { return .failure(.noCalendar) }
        let when: ResolvedWhen
        switch self.when(d, today: today, calendar: calendar) {
        case .failure(let p): return .failure(p)
        case .success(let w): when = w
        }
        var invited: [String]?
        if guests.open {
            let list = EventGuests.read(d.guests)
            if let p = EventGuests.problem(list, owner: guests.owner) { return .failure(p) }
            if !list.addresses.isEmpty { invited = list.addresses }
        }
        let notes = trimmed(d.notes)
        if length(notes) > maxNotes { return .failure(.notesTooLong) }
        let location = trimmed(d.location)
        let (start, end) = wireTimes(when, calendar)
        return .success(
            EventWrite(
                calendar_id: calendarId, title: title, start: start, end: end, all_day: when.allDay,
                location: location.isEmpty ? nil : location, description: notes.isEmpty ? nil : notes,
                guests: invited))
    }

    /// An edit's body: only the fields that moved from what the card opened
    /// with. The three time fields go together or not at all (the daemon
    /// refuses mixed precision), and only when one of the time fields was
    /// touched, so an event the form cannot fully say (a timed run of days)
    /// keeps its times when only its title changes. Empty means nothing
    /// changed.
    ///
    /// GUESTS go only when the line is open (`guests.open`) AND who is on it
    /// changed, and then as the WHOLE new list: the daemon replaces the
    /// event's guests with it. Order and case are not a change.
    static func patch(
        _ d: EventDraft, from initial: EventDraft, today: Date, calendar: Calendar,
        guests: GuestTerms = .closed
    ) -> Result<EventPatch, EventDraftProblem> {
        var p = EventPatch()
        let title = trimmed(d.title)
        if title != trimmed(initial.title) {
            if let problem = titleProblem(title) { return .failure(problem) }
            p.title = title
        }
        let timeMoved =
            d.allDay != initial.allDay || trimmed(d.date) != trimmed(initial.date)
            || trimmed(d.endDate) != trimmed(initial.endDate)
            || (!d.allDay && (trimmed(d.start) != trimmed(initial.start) || trimmed(d.end) != trimmed(initial.end)))
        if timeMoved {
            switch when(d, today: today, calendar: calendar) {
            case .failure(let problem): return .failure(problem)
            case .success(let w):
                (p.start, p.end) = wireTimes(w, calendar)
                p.all_day = w.allDay
            }
        }
        if guests.open {
            let list = EventGuests.read(d.guests)
            if let problem = EventGuests.problem(list, owner: guests.owner) { return .failure(problem) }
            if list.keys != EventGuests.read(initial.guests).keys { p.guests = list.addresses }
        }
        let location = trimmed(d.location)
        if location != trimmed(initial.location) {
            p.location = location.isEmpty ? .clear : .set(location)
        }
        let notes = trimmed(d.notes)
        if notes != trimmed(initial.notes) {
            if length(notes) > maxNotes { return .failure(.notesTooLong) }
            p.description = notes.isEmpty ? .clear : .set(notes)
        }
        return .success(p)
    }

    static func wireTimes(_ w: ResolvedWhen, _ calendar: Calendar) -> (start: String, end: String) {
        w.allDay
            ? (ymd(w.start, calendar), ymd(w.end, calendar))
            : (rfc3339(w.start, calendar), rfc3339(w.end, calendar))
    }

    // MARK: saying it back

    /// What the card will save, under the fields: "Thu Oct 8, 7:45pm to
    /// 8:45pm", "Thu Oct 8, 10pm to 2am Fri", "Thu Oct 8, all day", "Wed Oct
    /// 14 to Mon Oct 19, all day".
    static func summary(_ w: ResolvedWhen, calendar: Calendar) -> String {
        let first = Agenda.format(w.start, "EEE MMM d", calendar)
        if w.allDay {
            if calendar.isDate(w.start, inSameDayAs: w.end) { return "\(first), all day" }
            return "\(first) to \(Agenda.format(w.end, "EEE MMM d", calendar)), all day"
        }
        let start = Agenda.clock(w.start, calendar)
        let end = Agenda.clock(w.end, calendar)
        if calendar.isDate(w.start, inSameDayAs: w.end) { return "\(first), \(start) to \(end)" }
        let startDay = calendar.startOfDay(for: w.start)
        let nextDay = addDays(1, startDay, calendar)
        let endPattern = calendar.isDate(w.end, inSameDayAs: nextDay) ? "EEE" : "EEE MMM d"
        return "\(first), \(start) to \(end) \(Agenda.format(w.end, endPattern, calendar))"
    }

    // MARK: parsing dates

    /// A typed day, as a local midnight. Reads "today", "tomorrow", a weekday
    /// ("fri": the next one, today included), "Oct 8", "8 Oct", "October 8
    /// 2027", "Thu Oct 8" (the weekday is ignored when a date follows),
    /// "10/8", "10/8/27", "2026-10-08". Month first, as the app's dates are
    /// American English throughout. With no year: this year, unless that is
    /// more than three months gone, then next year.
    static func parseDate(_ raw: String, today: Date, calendar: Calendar) -> Date? {
        let today = calendar.startOfDay(for: today)
        var words = raw.lowercased()
            .replacingOccurrences(of: ",", with: " ")
            .split(whereSeparator: \.isWhitespace)
            .map(String.init)
        guard !words.isEmpty else { return nil }
        if words.count == 1 {
            switch words[0] {
            case "today", "tod": return today
            case "tomorrow", "tmrw", "tom": return addDays(1, today, calendar)
            case "yesterday": return addDays(-1, today, calendar)
            default: break
            }
        }
        if words.count > 1, weekday(words[0]) != nil { words.removeFirst() }
        if words.count == 1, let wd = weekday(words[0]) {
            let ahead = (wd - calendar.component(.weekday, from: today) + 7) % 7
            return addDays(ahead, today, calendar)
        }
        var month: Int?
        var day: Int?
        var year: Int?
        if words.count == 1 {
            let w = words[0]
            let iso = w.split(separator: "-").map(String.init)
            if iso.count == 3, iso[0].count == 4 {
                (year, month, day) = (Int(iso[0]), Int(iso[1]), Int(iso[2]))
                guard year != nil, month != nil, day != nil else { return nil }
            } else {
                let parts = w.split(separator: w.contains("/") ? "/" : "-").map(String.init)
                guard parts.count == 2 || parts.count == 3, parts.allSatisfy({ Int($0) != nil }) else { return nil }
                (month, day) = (Int(parts[0]), Int(parts[1]))
                if parts.count == 3 { year = Int(parts[2]) }
            }
        } else if words.count == 2 || words.count == 3 {
            if let m = monthNumber(words[0]), let d = dayNumber(words[1]) {
                (month, day) = (m, d)
            } else if let d = dayNumber(words[0]), let m = monthNumber(words[1]) {
                (month, day) = (m, d)
            } else {
                return nil
            }
            if words.count == 3 {
                guard let y = Int(words[2]) else { return nil }
                year = y
            }
        } else {
            return nil
        }
        guard let m = month, let d = day else { return nil }
        if let y = year, y < 100 { year = 2000 + y }
        if let y = year { return make(y, m, d, calendar) }
        let thisYear = calendar.component(.year, from: today)
        guard let candidate = make(thisYear, m, d, calendar) ?? make(thisYear + 1, m, d, calendar) else { return nil }
        if candidate < addDays(-92, today, calendar) { return make(thisYear + 1, m, d, calendar) }
        return candidate
    }

    private static func make(_ y: Int, _ m: Int, _ d: Int, _ calendar: Calendar) -> Date? {
        guard (1...12).contains(m), (1...31).contains(d), (1900...2999).contains(y) else { return nil }
        let date = calendar.date(from: DateComponents(year: y, month: m, day: d))
        // Feb 30 is not a date; the calendar would quietly make it Mar 2.
        guard let date, calendar.component(.day, from: date) == d, calendar.component(.month, from: date) == m
        else { return nil }
        return calendar.startOfDay(for: date)
    }

    private static let months = [
        "january", "february", "march", "april", "may", "june", "july", "august", "september",
        "october", "november", "december",
    ]
    private static let weekdays = ["sunday", "monday", "tuesday", "wednesday", "thursday", "friday", "saturday"]

    /// 1...12 for a month's name or its first three letters or more.
    static func monthNumber(_ w: String) -> Int? {
        let w = w.trimmingCharacters(in: CharacterSet(charactersIn: "."))
        guard w.count >= 3 else { return nil }
        return months.firstIndex { $0.hasPrefix(w) }.map { $0 + 1 }
    }

    /// 1 (Sunday) ... 7, the calendar's numbering, for a weekday's name or
    /// its first three letters or more.
    static func weekday(_ w: String) -> Int? {
        let w = w.trimmingCharacters(in: CharacterSet(charactersIn: "."))
        guard w.count >= 3 else { return nil }
        return weekdays.firstIndex { $0.hasPrefix(w) }.map { $0 + 1 }
    }

    /// "8", "8th", "1st", "22nd", "3rd".
    private static func dayNumber(_ w: String) -> Int? {
        var s = w
        for suffix in ["st", "nd", "rd", "th"] where s.hasSuffix(suffix) {
            s.removeLast(2)
            break
        }
        guard let n = Int(s), (1...31).contains(n) else { return nil }
        return n
    }

    // MARK: parsing times

    /// A typed time: "7pm", "7:45 pm", "7.45pm", "745p", "7a", "19:30",
    /// "1930", "07:00", "noon", "midnight", and a bare "7". A bare hour from
    /// 7 to 11 is morning, 12 is noon, 1 to 6 afternoon: the hours people
    /// make plans in. A leading zero or an hour past 12 is a 24-hour clock.
    static func parseTime(_ raw: String) -> TimeOfDay? {
        var s = raw.lowercased()
            .replacingOccurrences(of: "a.m.", with: "am")
            .replacingOccurrences(of: "p.m.", with: "pm")
            .replacingOccurrences(of: " ", with: "")
            .replacingOccurrences(of: ".", with: ":")
        if s == "noon" { return TimeOfDay(minutes: 12 * 60, explicit: true) }
        if s == "midnight" { return TimeOfDay(minutes: 0, explicit: true) }
        var pm: Bool?
        for (suffix, isPM) in [("am", false), ("pm", true), ("a", false), ("p", true)] where s.hasSuffix(suffix) {
            s.removeLast(suffix.count)
            pm = isPM
            break
        }
        guard !s.isEmpty else { return nil }
        let hourText: String
        var minute = 0
        if s.contains(":") {
            let parts = s.split(separator: ":", omittingEmptySubsequences: false).map(String.init)
            guard parts.count == 2, parts[1].count == 2, let m = Int(parts[1]) else { return nil }
            hourText = parts[0]
            minute = m
        } else {
            switch s.count {
            case 1, 2: hourText = s
            case 3:
                hourText = String(s.prefix(1))
                guard let m = Int(s.suffix(2)) else { return nil }
                minute = m
            case 4:
                hourText = String(s.prefix(2))
                guard let m = Int(s.suffix(2)) else { return nil }
                minute = m
            default: return nil
            }
        }
        guard !hourText.isEmpty, hourText.allSatisfy(\.isNumber), let hour = Int(hourText),
            (0...59).contains(minute)
        else { return nil }
        if let pm {
            guard (1...12).contains(hour) else { return nil }
            return TimeOfDay(minutes: ((hour % 12) + (pm ? 12 : 0)) * 60 + minute, explicit: true)
        }
        guard (0...23).contains(hour) else { return nil }
        if hour == 0 || hour > 12 || hourText.hasPrefix("0") {
            return TimeOfDay(minutes: hour * 60 + minute, explicit: true)
        }
        let h24 = hour == 12 ? 12 : (hour <= 6 ? hour + 12 : hour)
        return TimeOfDay(minutes: h24 * 60 + minute, explicit: false)
    }

    /// An end time, read against its start: a bare hour is the morning or
    /// the evening that comes SOONEST after the start ("9" after 7pm is 9pm,
    /// "2" after 10pm is 2am). Minutes after midnight.
    static func parseEnd(_ raw: String, after start: Int) -> Int? {
        guard let t = parseTime(raw) else { return nil }
        if t.explicit { return t.minutes }
        let am = t.minutes % 720
        let candidates = [am, am + 720]
        func ahead(_ m: Int) -> Int {
            let d = (m - start + 1440) % 1440
            return d == 0 ? 1440 : d
        }
        return candidates.min { ahead($0) < ahead($1) }
    }

    // MARK: text the card prints

    /// "Thu Oct 8", with the year when it is not this one.
    static func dateText(_ date: Date, today: Date, calendar: Calendar) -> String {
        let sameYear = calendar.component(.year, from: date) == calendar.component(.year, from: today)
        return Agenda.format(date, sameYear ? "EEE MMM d" : "EEE MMM d yyyy", calendar)
    }

    /// "9am", "7:45pm", from minutes after midnight.
    static func clockText(_ minutes: Int) -> String {
        let h24 = (minutes / 60) % 24
        let m = minutes % 60
        let hour = h24 % 12 == 0 ? 12 : h24 % 12
        let suffix = h24 < 12 ? "am" : "pm"
        return m == 0 ? "\(hour)\(suffix)" : "\(hour):\(String(format: "%02d", m))\(suffix)"
    }

    // MARK: wire text

    /// "2026-10-08T19:45:00-07:00": the instant in the user's zone, with that
    /// instant's own offset (a date across a clock change carries the other).
    static func rfc3339(_ date: Date, _ calendar: Calendar) -> String {
        let c = calendar.dateComponents([.year, .month, .day, .hour, .minute, .second], from: date)
        let offset = calendar.timeZone.secondsFromGMT(for: date)
        let sign = offset < 0 ? "-" : "+"
        let a = abs(offset)
        return String(
            format: "%04d-%02d-%02dT%02d:%02d:%02d%@%02d:%02d", c.year ?? 0, c.month ?? 0, c.day ?? 0,
            c.hour ?? 0, c.minute ?? 0, c.second ?? 0, sign, a / 3600, (a % 3600) / 60)
    }

    /// "2026-10-08", the date in the user's calendar.
    static func ymd(_ date: Date, _ calendar: Calendar) -> String {
        let c = calendar.dateComponents([.year, .month, .day], from: date)
        return String(format: "%04d-%02d-%02d", c.year ?? 0, c.month ?? 0, c.day ?? 0)
    }

    // MARK: helpers

    /// A wall time on a day, from components: `date(bySettingHour:)` searches
    /// forward and can hand back the next day (RemindTimes says why).
    static func at(_ day: Date, _ minutes: Int, _ calendar: Calendar) -> Date? {
        var c = calendar.dateComponents([.year, .month, .day], from: day)
        c.hour = minutes / 60
        c.minute = minutes % 60
        return calendar.date(from: c)
    }

    static func addDays(_ n: Int, _ day: Date, _ calendar: Calendar) -> Date {
        calendar.startOfDay(for: calendar.date(byAdding: .day, value: n, to: day) ?? day)
    }

    private static func minutesOfDay(_ date: Date, _ calendar: Calendar) -> Int {
        let c = calendar.dateComponents([.hour, .minute], from: date)
        return (c.hour ?? 0) * 60 + (c.minute ?? 0)
    }
}
