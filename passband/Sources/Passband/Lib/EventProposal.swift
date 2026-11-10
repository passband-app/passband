// THE ASSISTANT PROPOSES, THE PERSON ADDS: the assistant can create events,
// but only as a draft the person confirms.
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
// THE TITLE AND THE PLACE ARE THE ONLY TEXT IT SUPPLIES, each one line of at
// most 200 characters, and over that is a refusal the model can correct,
// never a trim: with guests on an event Google emails both to them, and both
// sit in one-line fields of the pane, where anything longer would be cut off
// from the person about to press "add and invite". The tool takes no notes
// and no description: Passband never puts model-written prose in front of
// other people. A call that sends notes anyway, under any name a model
// reaches for and in any case, is refused in words, and so is any other
// field the tool does not have: nothing is dropped in silence, so the model
// never tells the user something was saved that was not. The notes line of
// the pane it opens is always empty, for the user to type in.
//
// IT NEVER TAKES THE SCREEN FROM SOMEBODY (verify7 D2). A proposal is drafted
// only while the assistant's bar is open, which is where the person who
// asked for it is; never over mail being written; and never by closing the
// email being read (then it waits, parked, on the Calendar page). The pane
// it opens does not take the keyboard, and does nothing on ↵ or ⌘↵ until
// the user has clicked or typed in it: the keystroke that adds an event, or
// invites its guests, is one made at this pane, having seen it. Modelled on
// the agent door's `open_draft` (`ComposeState.keyboardSendLocked`).
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
    /// The calendar was asked for and did not answer. NOT "no calendar": a
    /// user whose calendar is connected must not be told to connect it.
    static let calendarUnreachableText =
        "Passband could not reach the user's calendar just now, so nothing was drafted. This is not a sign "
        + "that no calendar is connected. Tell the user it could not be reached, and try again if they ask."
    /// The assistant's bar is not open: whoever asked is no longer at it.
    static let askClosedText =
        "Nothing was drafted: the assistant's bar is closed, and Passband opens an event draft only while "
        + "the user is at it. Tell the user to ask again from the assistant's bar."
    /// Mail is being written: it is never closed for a proposal.
    static let writingMailText =
        "Nothing was drafted: the user is in the middle of writing an email, and Passband does not close "
        + "it to open an event. Ask them to send or close that email first, then try again."
    /// Drafted, with an email open: the pane waits on the Calendar page.
    static let draftedParkedText =
        "The event is drafted and waiting on the Calendar page, behind the email the user is reading, "
        + "which was left open. Nothing has been added to the calendar and nobody has been invited. Only "
        + "the user can add it, with the event pane's own control. Tell them it is waiting on the Calendar "
        + "page to review; do not say it was created or that anyone was invited."
    /// The conversation that asked is gone (New chat), or the mailbox was
    /// switched, while the calendar was being read (verify8 V8-1). Fixed
    /// words, and nothing opens.
    static let staleRunText =
        "Nothing was drafted: the conversation was cleared or the mailbox was switched while the calendar "
        + "was loading. Do not try again; the user will ask again if they still want the event."
    /// ⌘↵ or ↵ on a proposed pane nobody has clicked or typed in.
    static let keysLockedText = "Your assistant drafted this event. Read it, then click add."
    /// ⌘↵ on a proposed pane that has guests, clicked in or not.
    static let guestsNeedAClickText = "Your assistant added these guests. Only a click on the button invites them."
    /// The same, said in the pane's footer for as long as it holds.
    static let guestsNeedAClickHint = "Your assistant added these guests. Only a click on this button invites them; ⌘↵ does not."
    static let longTitleText =
        "The title must be one line of at most 200 characters. Shorten it and call again."
    static let longLocationText =
        "The location must be one line of at most 200 characters: a place name or an address, not a "
        + "description. Shorten it and call again."
    /// One-line text a proposal may carry: the daemon's own caps.
    static let maxLine = 200
    static let unknownCalendarText =
        "No calendar the user can add events to has that name. Leave calendar out to use their primary calendar."
    static let cannotInviteText =
        "Guests cannot be invited on that calendar from Passband. Leave guests out, or leave calendar out to use the primary calendar."
    static let noNotesText =
        "This tool takes no notes or description: Passband does not put assistant-written text in an event, "
        + "because Google emails an event's description to its guests. Call it again without them, and tell "
        + "the user they can type notes in the event pane themselves."
    /// The keys a model reaches for when it wants to write a description,
    /// matched whatever their case.
    static let notesKeys: Set<String> = [
        "notes", "note", "description", "body", "details", "agenda", "summary", "message",
    ]
    /// Every field the tool has. Any other key with something in it is
    /// refused by name.
    static let knownKeys: Set<String> = ["title", "start", "end", "all_day", "calendar", "location", "guests"]

    static func unknownFieldText(_ key: String) -> String {
        // The key is the model's own text: said back short, on one line.
        let name = String(key.unicodeScalars.filter { $0.properties.isAlphabetic || $0 == "_" }.prefix(40))
        return "unknown field \"\(name)\": this tool takes only title, start, end, all_day, calendar, location and guests. "
            + "Nothing was drafted. Call it again without that field."
    }
    static let questionOpenText =
        "The Calendar page is asking the user a question. Ask them to answer it first, then try again."

    /// The system prompt's one line about the tool, while it is offered.
    static let promptLine = """
        - create_calendar_event drafts an event in Passband's event pane for the user to
          confirm. It adds nothing to their calendar and invites nobody; only the user
          can, by pressing the pane's own control. Say it is drafted, never that it exists.
          The title and the place are the only text you supply, one line each: it takes
          no notes or description, and any other field is refused.
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
        // NOTHING IS DROPPED IN SILENCE. Notes in any spelling and any case
        // are refused as notes; any other key the tool does not have is
        // refused by name. Sorted, so the same call gets the same answer. A
        // key holding nothing (null, "") carried nothing and is let pass.
        let extra = input.keys.sorted().filter { key in
            guard !knownKeys.contains(key), let value = input[key] else { return false }
            return value != .null && value != .string("")
        }
        if extra.contains(where: { notesKeys.contains($0.lowercased()) }) {
            return .failure(ProposalRefusal(text: noNotesText))
        }
        if let key = extra.first { return .failure(ProposalRefusal(text: unknownFieldText(key))) }
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
                calendar: string("calendar"), location: string("location"), guests: guests))
    }

    /// One line, of at most `maxLine` characters as the daemon counts them:
    /// no line break, tab or other control character anywhere in it.
    static func oneLine(_ text: String) -> Bool {
        EventForm.length(text) <= maxLine
            && !text.unicodeScalars.contains {
                CharacterSet.newlines.contains($0) || CharacterSet.controlCharacters.contains($0)
            }
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
    ///
    /// NO FEED IS NOT "NO CALENDAR" (verify7 E12). The feed is loaded by the
    /// Calendar page, so before that page has been opened there is none; the
    /// tool loads it once itself, and a nil feed after that is a load that
    /// failed. "No calendar" is said only of a feed that arrived and lists
    /// nothing writable.
    static func calendar(_ named: String?, feed: TimelineFeed?) -> Result<TimelineCalendar, ProposalRefusal> {
        guard let feed else { return .failure(ProposalRefusal(text: calendarUnreachableText)) }
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
        // THE ONLY TEXT THE MODEL SUPPLIES: one line each, capped, and over
        // the cap is refused, never trimmed (verify7 D3, E14). Checked here,
        // on the one road every proposal takes into a draft.
        guard oneLine(p.title) else { return .failure(ProposalRefusal(text: longTitleText)) }
        if let place = p.location, !oneLine(place) { return .failure(ProposalRefusal(text: longLocationText)) }
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
            editable: true, description: nil)
        var d = EventForm.draft(editing: event, today: today, calendar: calendar)
        d.calendarId = target.id
        // Never the assistant's: the notes line opens empty.
        d.notes = ""

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

    // MARK: the screen

    /// Whether the feed has to be loaded before a proposal is judged: only
    /// when there is none. A feed that is there, however old, is used as it
    /// is (the daemon decides at the save).
    static func mustLoad(feed: TimelineFeed?) -> Bool { feed == nil }

    /// Where a proposal's pane goes.
    enum Landing: Hashable, Sendable {
        /// On the Calendar page, beside the month.
        case calendarPage
        /// Kept whole and not drawn, for the Calendar page to show when the
        /// user goes there: an email is open, and it is not closed.
        case parked
    }

    /// Whether a proposal may be drafted now, and where its pane goes.
    ///
    /// - The assistant's bar must be OPEN at this moment. A run goes on after
    ///   the bar is closed (a citation clicked, "edit in composer"), and a
    ///   tool call that lands then would change the page under somebody who
    ///   is no longer looking at the assistant.
    /// - Mail being written (a composer or an inline reply holding anything
    ///   typed) is never closed for it.
    /// - An open email is never closed for it: the pane is parked.
    static func landing(askBarOpen: Bool, writingMail: Bool, threadOpen: Bool) -> Result<Landing, ProposalRefusal> {
        guard askBarOpen else { return .failure(ProposalRefusal(text: askClosedText)) }
        guard !writingMail else { return .failure(ProposalRefusal(text: writingMailText)) }
        return .success(threadOpen ? .parked : .calendarPage)
    }

    /// What the model is told of a proposal that was drafted.
    static func draftedText(_ landing: Landing) -> String {
        landing == .parked ? draftedParkedText : draftedText
    }

    /// Whether ↵ and ⌘↵ may act on the pane, and whether it may take the
    /// keyboard when it appears. A pane the ASSISTANT filled in does neither
    /// until the user has clicked or typed inside it (`engaged`): a key
    /// typed ahead, or meant for whatever was on screen a moment ago, must
    /// not add an event or invite its guests. The pane's own button always
    /// works: a click cannot be typed ahead.
    static func keysMayAct(proposed: Bool, engaged: Bool) -> Bool { !proposed || engaged }

    /// Whether ⌘↵ may SAVE the pane (verify8 V8-3). On a pane the assistant
    /// filled in whose save would tell guests, never, clicked in or not: one
    /// click on the title and then ⌘↵ is not a decision to email the people
    /// a model named. The invitation goes out only by a click on the control
    /// that names them ("add and invite 3"). With no guests, or on a pane
    /// the user opened, the rule is `keysMayAct`.
    static func commandEnterMaySave(proposed: Bool, engaged: Bool, tellsGuests: Bool) -> Bool {
        if proposed, tellsGuests { return false }
        return keysMayAct(proposed: proposed, engaged: engaged)
    }

    /// Whether the run that asked for a proposal is still the one to answer
    /// (verify8 V8-1): its conversation is still the live one, and the
    /// mailbox is the one it was asked in. Checked AFTER the calendar feed
    /// is read, which is the only wait on the tool's path: a New chat or an
    /// account switch during it must not leave a pane on screen from a
    /// conversation thrown away, or draft one mailbox's event on another's
    /// calendar.
    static func runStillCurrent(live: Bool, epochAsked: Int, epochNow: Int) -> Bool {
        live && epochAsked == epochNow
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
