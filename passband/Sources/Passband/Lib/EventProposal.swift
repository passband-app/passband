// THE ASSISTANT PROPOSES, THE PERSON ADDS (wave 6, the founder: "lets give
// the assistant the ability to create events").
//
// The in-app assistant has ONE calendar tool, `create_calendar_event`, and it
// writes nothing. It turns what the model asked for into the event pane's own
// draft (Lib/EventDraft.swift), checked by the pane's own rules, and the
// store opens the pane with it filled in. The user reads it and presses "add
// event" or "add and invite N" themselves, exactly as if they had typed it.
// The same rule as mail: the assistant drafts, the person sends. Nothing here
// calls the daemon, and no invitation can leave without that press.
//
// PURE, and Foundation only (Tests/EventProposalTests.swift): reading the
// tool's arguments, choosing the calendar, building and checking the draft,
// when a pane already open refuses a new proposal, and which mailboxes are
// offered the tool at all.
//
// THE ASSISTANT DOES NOT READ THE CALENDAR. A proposal names a calendar or
// leaves it to the primary one; a name that matches nothing is refused
// without listing the calendars there are.

import Foundation

/// What the model asked for, as read from the tool call.
struct EventProposal: Hashable, Sendable {
    var title: String
    /// Timed: a date and time, with or without an offset. All day: a date.
    var start: String
    var end: String?
    var allDay = false
    /// A calendar's id or name; nil is the primary.
    var calendar: String?
    var location: String?
    var notes: String?
    var guests: [String] = []
}

/// Why a proposal was not drafted, in words the model can act on. Never
/// echoes the calendar's contents.
struct ProposalRefusal: Error, Hashable, Sendable {
    let text: String
}

enum EventProposals {
    /// The tool's wire name.
    static let toolName = "create_calendar_event"

    // MARK: who is offered it

    /// The assistant's tools for one mailbox: `tools` exactly as given while
    /// the Calendar page is off, and with the calendar tool after `after`
    /// (or last) while it is on. With the page off the list, and so the
    /// schema the model is sent, is the one from before the tool existed.
    static func inventory(_ tools: [String], calendar: Bool, after: String) -> [String] {
        let base = tools.filter { $0 != toolName }
        guard calendar else { return base }
        var out = base
        let at = base.firstIndex(of: after).map { $0 + 1 } ?? base.count
        out.insert(toolName, at: at)
        return out
    }

    /// Whether a call to `name` may run. The calendar tool only while the
    /// page is on: off, it is a tool that does not exist, and is answered as
    /// any invented name is.
    static func mayRun(_ name: String, calendar: Bool) -> Bool {
        name != toolName || calendar
    }

    // MARK: what it answers

    /// The one success. It says what did NOT happen, because a model told
    /// only "drafted" will tell the user the event is on their calendar.
    static let draftedText =
        "The event is drafted in the event pane and waiting for the user to confirm. Nothing has been "
        + "added to the calendar and nobody has been invited. Only the user can add it, with the pane's "
        + "own control. Tell them it is ready to review; do not say it was created or that anyone was invited."

    static let paneBusyText =
        "An event is already open in the event pane with unsaved changes. Ask the user to finish or close "
        + "the open event first, then try again."
    static let noCalendarText =
        "This mailbox has no calendar Passband can add events to. The user may need to connect Google Calendar on the Calendar page."
    static let unknownCalendarText =
        "No calendar the user can add events to has that name. Leave calendar out to use their primary calendar."
    static let cannotInviteText =
        "Guests cannot be invited on that calendar from Passband. Leave guests out, or leave calendar out to use the primary calendar."
    static let questionOpenText =
        "The Calendar page is asking the user a question. Ask them to answer it first, then try again."

    /// The system prompt's one line about the tool, while it is offered.
    static let promptLine = """
        - create_calendar_event drafts an event in Passband's event pane for the user to
          confirm. It adds nothing to their calendar and invites nobody; only the user
          can, by pressing the pane's own control. Say it is drafted, never that it exists.
        """

    // MARK: reading the call

    /// The tool call's arguments. Tolerant about shape the way the other
    /// tools are (a quoted boolean, a lone guest as a string), strict about
    /// what is missing.
    static func read(_ input: [String: JSONValue]) -> Result<EventProposal, ProposalRefusal> {
        func string(_ key: String) -> String? {
            guard let v = input[key]?.stringValue?.trimmingCharacters(in: .whitespacesAndNewlines), !v.isEmpty
            else { return nil }
            return v
        }
        guard let title = string("title") else { return .failure(ProposalRefusal(text: "missing title")) }
        guard let start = string("start") else { return .failure(ProposalRefusal(text: "missing start")) }
        var allDay = input["all_day"]?.boolValue
        if allDay == nil, let text = input["all_day"]?.stringValue?.lowercased() {
            if text == "true" { allDay = true }
            if text == "false" { allDay = false }
        }
        var guests: [String] = []
        if let list = input["guests"]?.arrayValue {
            // Every element has to be a string: a list with something else in
            // it is not a guest list to guess at.
            guard list.allSatisfy({ $0.stringValue != nil }) else {
                return .failure(ProposalRefusal(text: "guests must be a list of email addresses"))
            }
            guests = list.compactMap { $0.stringValue?.trimmingCharacters(in: .whitespacesAndNewlines) }
                .filter { !$0.isEmpty }
        } else if let one = string("guests") {
            guests = EventGuests.split(one)
        } else if let other = input["guests"], other != .null {
            return .failure(ProposalRefusal(text: "guests must be a list of email addresses"))
        }
        return .success(
            EventProposal(
                title: title, start: start, end: string("end"), allDay: allDay ?? false,
                calendar: string("calendar"), location: string("location"), notes: string("notes"),
                guests: guests))
    }

    // MARK: the draft

    /// "2026-10-09": a date and nothing else.
    static func isDate(_ s: String) -> Bool {
        let parts = s.split(separator: "-", omittingEmptySubsequences: false)
        return s.count == 10 && parts.count == 3 && parts[0].count == 4 && parts[1].count == 2
            && parts[2].count == 2 && parts.allSatisfy { $0.allSatisfy(\.isNumber) }
    }

    /// The calendar a proposal goes on: the primary writable one when none
    /// is named (or "primary" is), else the writable calendar with that id
    /// or, folded, that name.
    static func calendar(_ named: String?, feed: TimelineFeed?) -> Result<TimelineCalendar, ProposalRefusal> {
        let writable = CalendarWrite.writable(feed)
        guard let first = writable.first else { return .failure(ProposalRefusal(text: noCalendarText)) }
        guard let named, named.lowercased() != "primary" else { return .success(first) }
        if let byId = writable.first(where: { $0.id == named }) { return .success(byId) }
        let wanted = named.lowercased()
        if let byName = writable.first(where: { CalendarWrite.name($0).lowercased() == wanted }) {
            return .success(byName)
        }
        return .failure(ProposalRefusal(text: unknownCalendarText))
    }

    /// The pane's draft for a proposal, or why there is none. EVERYTHING the
    /// pane would check is checked here first (`EventForm.wire`, with the
    /// guests line open exactly when this calendar may invite), so a bad
    /// proposal is an answer the model can correct and never a half-filled
    /// pane. The draft's title is NOT read for a day or a time: the proposal
    /// already says when.
    static func draft(
        _ p: EventProposal, feed: TimelineFeed?, owner: String?, today: Date, calendar: Calendar
    ) -> Result<EventDraft, ProposalRefusal> {
        let target: TimelineCalendar
        switch self.calendar(p.calendar, feed: feed) {
        case .failure(let refusal): return .failure(refusal)
        case .success(let c): target = c
        }

        // WHEN, in the wire's own two forms.
        let start: String
        let end: String
        var instants: (Date, Date)?
        if p.allDay {
            guard isDate(p.start), AgendaTime.parse(p.start, zone: nil, calendar: calendar) != nil else {
                return .failure(ProposalRefusal(text: "an all-day event's start must be a date like 2026-10-09"))
            }
            let last = p.end ?? p.start
            guard isDate(last), AgendaTime.parse(last, zone: nil, calendar: calendar) != nil else {
                return .failure(
                    ProposalRefusal(text: "an all-day event's end must be its last day, a date like 2026-10-11"))
            }
            guard last >= p.start else { return .failure(ProposalRefusal(text: "the event ends before it starts")) }
            (start, end) = (p.start, last)
        } else {
            guard !isDate(p.start), let s = AgendaTime.parse(p.start, zone: nil, calendar: calendar), !s.dateOnly
            else {
                return .failure(
                    ProposalRefusal(
                        text: "start must be a date and time like 2026-10-09T16:00:00, or set all_day to true for a date"))
            }
            var e = s.date.addingTimeInterval(3600)
            if let raw = p.end {
                guard !isDate(raw), let parsed = AgendaTime.parse(raw, zone: nil, calendar: calendar), !parsed.dateOnly
                else {
                    return .failure(ProposalRefusal(text: "end must be a date and time like 2026-10-09T17:00:00"))
                }
                e = parsed.date
            }
            guard e > s.date else { return .failure(ProposalRefusal(text: "the event must end after it starts")) }
            (start, end) = (EventForm.rfc3339(s.date, calendar), EventForm.rfc3339(e, calendar))
            instants = (s.date, e)
        }

        // The same road an edit takes into the form, so the fields read
        // exactly as the pane prints its own.
        let event = TimelineEvent(
            id: "", calendar_id: target.id, calendar_name: target.name, title: p.title, start: start, end: end,
            all_day: p.allDay, start_timezone: nil, location: p.location, status: .confirmed,
            self_response: nil, busy: true, organizer: nil, html_link: nil, color: target.color,
            editable: true, description: p.notes)
        var d = EventForm.draft(editing: event, today: today, calendar: calendar)
        d.calendarId = target.id

        // GUESTS, as whole pills: the user asked the assistant for them. Only
        // where this calendar may invite at all.
        let access = EventGuests.access(calendarId: target.id, editing: nil, feed: feed)
        if !p.guests.isEmpty {
            guard access == .field else { return .failure(ProposalRefusal(text: cannotInviteText)) }
            guard p.guests.allSatisfy({ EventGuests.isAddress(EventGuests.bare($0)) }) else {
                return .failure(ProposalRefusal(text: "every guest must be a whole email address"))
            }
            d.guests = p.guests.map(EventGuests.bare).joined(separator: ", ") + ","
        } else {
            d.guests = ""
        }

        // The pane's own verdict.
        let terms = GuestTerms(open: access == .field, owner: owner)
        if case .failure(let problem) = EventForm.wire(d, today: today, calendar: calendar, guests: terms) {
            return .failure(ProposalRefusal(text: problem.text))
        }
        // And the form must say back the instants that were asked for: a
        // time the fields cannot hold is refused, not rounded.
        if let (s, e) = instants {
            guard case .success(let w) = EventForm.when(d, today: today, calendar: calendar),
                abs(w.start.timeIntervalSince(s)) < 60, abs(w.end.timeIntervalSince(e)) < 60
            else {
                return .failure(ProposalRefusal(text: "that time could not be put in the event form; use whole minutes"))
            }
        }
        return .success(d)
    }

    // MARK: the pane

    /// Why a proposal may not open the pane now, or nil. A pane with
    /// anything in it (typed by the user, or proposed before and not yet
    /// answered), on screen or parked on another page, is never replaced,
    /// and neither is one whose write is out or in doubt. An untouched pane
    /// simply gives way.
    static func paneRefusal(paneExists: Bool, touched: Bool, editable: Bool, deleteAsked: Bool) -> String? {
        if deleteAsked { return questionOpenText }
        guard paneExists else { return nil }
        return touched || !editable ? paneBusyText : nil
    }
}
