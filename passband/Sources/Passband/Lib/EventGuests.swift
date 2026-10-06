// GUESTS ON AN EVENT (wave 5): who the pane's guests line names, which
// calendars may invite, which save controls a draft shows, and the one rule
// this file exists for: WHEN GOOGLE EMAILS OTHER PEOPLE.
//
// The invitation, the update and the cancellation are Google Calendar's own
// mail, sent by Google when a write says `notify: true`. Passband writes none
// of it. It cannot be unsent, so:
//
//   - `notify: true` is decided in ONE place, `EventGuests.notify`, and only
//     for a control that says so in its own words ("add and invite 3", "save
//     and tell guests") AND is on offer for the draft as it stands. Every
//     other path, a key, a default, a retry, sends no `notify` at all, which
//     the daemon reads as "tell nobody".
//   - Plain Enter never saves a draft whose save would tell anyone
//     (`enterMaySave`). Only the control, or its ⌘↵, does.
//   - A guest list the daemon holds only part of (`guests_truncated`), or one
//     that is not the user's to change, is never sent back: the pane shows it
//     and the patch leaves it out (`GuestAccess`, `GuestTerms.open`).
//
// PURE, and Foundation only. Tests/EventGuestsTests.swift holds each rule.

import Foundation

// MARK: - the list, as typed

/// The guests line, read: the addresses it names, and whatever on it is not
/// an address yet.
struct GuestList: Hashable, Sendable {
    /// Bare addresses, one per person (compared case-folded), in the order
    /// typed and in the first spelling given.
    var addresses: [String] = []
    /// Tokens that are not an address: half typed, or never one.
    var unread: [String] = []

    var keys: Set<String> { Set(addresses.map(EventGuests.key)) }
}

/// What the guests line may do for the draft on screen.
enum GuestAccess: Hashable, Sendable {
    /// The line is a field: people can be added and removed.
    case field
    /// Nothing can be changed here, and the line says why. Any guests the
    /// event has are listed, read-only.
    case closed(GuestBar)
    /// No line at all: a daemon older than guests.
    case none
}

/// Why the guests line is closed, in the pane's own words.
enum GuestBar: Hashable, Sendable {
    case otherCalendar
    case notOrganizer
    case tooMany

    var text: String {
        switch self {
        case .otherCalendar: "Guests can be invited from your own calendar only."
        case .notOrganizer: "Only the organizer can change the guests."
        case .tooMany: "This event has more guests than Passband can change here."
        }
    }
}

/// What a write may say about guests. `open` false (the default everywhere)
/// means the body carries no `guests` key at all.
struct GuestTerms: Hashable, Sendable {
    var open = false
    /// The account's own address: the calendar's owner is on the event
    /// already and is never a guest of it.
    var owner: String?

    static let closed = GuestTerms()
}

// MARK: - the save controls

/// A control in the pane's footer. Its label is what it does, and the two
/// that make Google email people say so.
enum SaveControl: Hashable, Sendable {
    case addEvent
    case addAndInvite(Int)
    case save
    case saveAndTell
    case saveWithoutTelling

    /// Google emails the guests when this control saves.
    var notifies: Bool {
        switch self {
        case .addAndInvite, .saveAndTell: true
        case .addEvent, .save, .saveWithoutTelling: false
        }
    }

    var label: String {
        switch self {
        case .addEvent: "add event"
        case .addAndInvite(let n): "add and invite \(n)"
        case .save: "save"
        case .saveAndTell: "save and tell guests"
        case .saveWithoutTelling: "save without telling"
        }
    }
}

/// The footer for a draft: the control ⌘↵ presses, and the quieter one
/// beside it when there are two ways to save.
struct SaveControls: Hashable, Sendable {
    let primary: SaveControl
    var quiet: SaveControl?

    var all: [SaveControl] { [primary] + (quiet.map { [$0] } ?? []) }
}

/// Everything the pane and the store need to know about a draft's guests,
/// worked out once from the same three things.
struct GuestPlan: Hashable, Sendable {
    let access: GuestAccess
    /// Guests the event had when the pane opened (the user not counted).
    let before: Int
    /// Guests the save would leave on it.
    let after: Int
    let controls: SaveControls

    /// What a write built for this draft may carry.
    func terms(owner: String?) -> GuestTerms {
        GuestTerms(open: access == .field, owner: owner)
    }
}

/// The delete question, by how many people it would tell.
struct DeleteWording: Hashable, Sendable {
    let title: String
    let detail: String
    /// The control that deletes and has Google tell the guests. Nil when the
    /// event has none.
    let tell: String?
    /// The control that deletes and tells nobody.
    let quiet: String
}

/// One line of the detail pane's guest list.
struct GuestRow: Hashable, Sendable, Identifiable {
    let id: String
    let name: String
    /// "accepted", "declined", "maybe", "no reply".
    let response: String
    let organizer: Bool
}

enum EventGuests {
    /// The daemon's cap on one event's guests.
    static let cap = 50

    // MARK: reading the line

    /// An address's identity: case-folded.
    static func key(_ address: String) -> String {
        address.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
    }

    /// The address inside a token, as spelled: `Sam Lee <sam@x.test>` gives
    /// what is between the last angle pair, anything else gives itself.
    /// (`Recipients.key`'s rule, without the case fold.)
    static func bare(_ token: String) -> String {
        let t = token.trimmingCharacters(in: .whitespacesAndNewlines)
        if let open = t.lastIndex(of: "<"), let close = t.lastIndex(of: ">"), open < close {
            return String(t[t.index(after: open)..<close]).trimmingCharacters(in: .whitespacesAndNewlines)
        }
        return t
    }

    /// A plain addr-spec, as the daemon takes one: ASCII, a mailbox, one `@`,
    /// a dotted host, and none of the characters a display name or a list
    /// would bring. The daemon has the last word.
    static func isAddress(_ s: String) -> Bool {
        guard !s.isEmpty, s.unicodeScalars.count <= 254, s.unicodeScalars.allSatisfy(\.isASCII) else { return false }
        let banned = CharacterSet(charactersIn: "<>()[],;:\\\"").union(.whitespacesAndNewlines).union(.controlCharacters)
        guard s.unicodeScalars.allSatisfy({ !banned.contains($0) }) else { return false }
        let parts = s.split(separator: "@", omittingEmptySubsequences: false)
        guard parts.count == 2, !parts[0].isEmpty, parts[0].count <= 64 else { return false }
        let labels = parts[1].split(separator: ".", omittingEmptySubsequences: false)
        return labels.count >= 2 && labels.allSatisfy { !$0.isEmpty }
    }

    /// The guests line, read. The line is the recipient field's own text
    /// (pills joined by commas, a half-typed fragment last), so it is split
    /// by the composer's grammar: a comma inside a quoted name is the name's.
    /// Lines split first, so a pasted column of addresses is a list too.
    static func read(_ text: String) -> GuestList {
        var list = GuestList()
        var seen = Set<String>()
        for line in text.split(whereSeparator: \.isNewline) {
            for token in split(String(line)) {
                let address = bare(token)
                guard isAddress(address) else {
                    list.unread.append(token)
                    continue
                }
                if seen.insert(key(address)).inserted { list.addresses.append(address) }
            }
        }
        return list
    }

    /// `Recipients.split`, restated here so this file stands alone under
    /// test: commas separate, except inside a quoted display name.
    static func split(_ list: String) -> [String] {
        var out: [String] = []
        var current = ""
        var quoted = false
        for ch in list {
            if ch == "\"" { quoted.toggle() }
            if ch == ",", !quoted {
                out.append(current)
                current = ""
            } else {
                current.append(ch)
            }
        }
        out.append(current)
        return out.map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }.filter { !$0.isEmpty }
    }

    /// The first thing wrong with the line, in the order it is noticed: a
    /// token that is no address, the user's own address, too many.
    static func problem(_ list: GuestList, owner: String?) -> EventDraftProblem? {
        if !list.unread.isEmpty { return .badGuest }
        if let owner, !key(owner).isEmpty, list.keys.contains(key(owner)) { return .guestIsYou }
        if list.addresses.count > cap { return .tooManyGuests }
        return nil
    }

    // MARK: an event's guests

    /// The people on an event other than the user: who an invitation, an
    /// update or a cancellation would go to.
    static func others(_ event: TimelineEvent?) -> [TimelineGuest] {
        (event?.guests ?? []).filter { $0.isSelf != true }
    }

    /// How many people a write to this event would tell. A truncated list
    /// counts as at least one: there are guests, however few were stored.
    static func count(_ event: TimelineEvent?) -> Int {
        let n = others(event).count
        return n == 0 && event?.guests_truncated == true ? 1 : n
    }

    /// The guests line an edit opens with: the others' addresses, as Google
    /// has them.
    static func fieldText(_ event: TimelineEvent) -> String {
        others(event).map(\.email).joined(separator: ", ")
    }

    /// The others on an event in one line, for a guests line that is closed:
    /// "Sam Lee, ana@x.test and 3 more". Nil with nobody to name.
    static func names(_ event: TimelineEvent, limit: Int = 3) -> String? {
        let all = others(event).map { g -> String in
            let name = (g.name ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
            return name.isEmpty ? g.email : name
        }
        guard !all.isEmpty else { return nil }
        let shown = all.prefix(limit).joined(separator: ", ")
        let hidden = all.count - min(limit, all.count)
        if hidden > 0 { return "\(shown) and \(hidden) more" }
        return event.guests_truncated == true ? "\(shown) and others" : shown
    }

    // MARK: who may invite

    /// Whether this daemon knows about guests at all. It says so by serving
    /// `guest_editable` on its events (or `can_invite` on a calendar); an
    /// older daemon serves neither, would drop a `guests` key unread, and so
    /// is never sent one.
    static func daemonInvites(_ feed: TimelineFeed?) -> Bool {
        (feed?.events ?? []).contains { $0.guest_editable != nil }
            || (feed?.calendars ?? []).contains { $0.can_invite != nil }
    }

    /// A calendar events may carry guests on: the user's own. The daemon's
    /// word (`can_invite`: primary, or one the user owns), which every daemon
    /// that knows guests serves. Without it, the primary calendar, and only
    /// on a daemon that has shown some other way that it can invite: one
    /// that has not is never offered a guests line, because it would save
    /// the event and drop the guests while the control said "invite".
    static func mayInvite(_ calendar: TimelineCalendar?, feed: TimelineFeed?) -> Bool {
        guard let calendar, calendar.writable == true else { return false }
        if let said = calendar.can_invite { return said }
        return calendar.primary == true && daemonInvites(feed)
    }

    /// What the guests line is for this draft. A new event follows the
    /// calendar picked. An edit follows the event first: its list must be
    /// whole and the user's to change, and then its calendar must be one
    /// that may invite.
    static func access(calendarId: String?, editing event: TimelineEvent?, feed: TimelineFeed?) -> GuestAccess {
        let calendar = feed?.calendars?.first { $0.id == (event?.calendar_id ?? calendarId) }
        guard let event else {
            guard daemonInvites(feed) else { return .none }
            return mayInvite(calendar, feed: feed) ? .field : .closed(.otherCalendar)
        }
        guard let editable = event.guest_editable else { return .none }
        if event.guests_truncated == true { return .closed(.tooMany) }
        if !editable { return .closed(.notOrganizer) }
        return mayInvite(calendar, feed: feed) ? .field : .closed(.otherCalendar)
    }

    // MARK: the controls

    /// The footer for a draft. A new event with guests has ONE control, and
    /// it says it invites. An event with guests on either side of an edit has
    /// two, so telling them is always chosen and never assumed.
    static func controls(editing: Bool, before: Int, after: Int) -> SaveControls {
        if !editing {
            return SaveControls(primary: after > 0 ? .addAndInvite(after) : .addEvent)
        }
        if before > 0 || after > 0 {
            return SaveControls(primary: .saveAndTell, quiet: .saveWithoutTelling)
        }
        return SaveControls(primary: .save)
    }

    static func plan(_ draft: EventDraft, editing event: TimelineEvent?, feed: TimelineFeed?) -> GuestPlan {
        let access = access(calendarId: draft.calendarId, editing: event, feed: feed)
        let before = count(event)
        // Only an open line changes who is on it. A closed one leaves the
        // event's guests as they were; a new event on a closed line has none.
        let after = access == .field ? read(draft.guests).addresses.count : before
        return GuestPlan(
            access: access, before: before, after: after,
            controls: controls(editing: event != nil, before: before, after: after))
    }

    /// ENTER DOES NOT SEND. Plain Enter in a single-line field saves only
    /// when there is one way to save and it tells nobody. With a guest on
    /// either side of the save, only ⌘↵ or a click does.
    static func enterMaySave(_ controls: SaveControls) -> Bool {
        controls.quiet == nil && !controls.primary.notifies
    }

    /// NOTIFY ONLY FROM THE LABELLED CONTROL. Whether a save asked for by
    /// `asked` tells the guests: true only for a control that says so, and
    /// only if that control is on offer for the draft as it stands now. Nil
    /// when it is not on offer (the draft changed under the click): then
    /// nothing is saved at all.
    static func notify(_ asked: SaveControl, offered: SaveControls) -> Bool? {
        guard offered.all.contains(asked) else { return nil }
        return asked.notifies
    }

    // MARK: words

    /// The one sentence about email, under the guests line. Nil with nobody
    /// to tell.
    static func emailLine(editing: Bool, guests: Int, account: String?) -> String? {
        guard guests > 0 else { return nil }
        let from = account.map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }.flatMap { $0.isEmpty ? nil : $0 }
        let tail = from.map { " from \($0)" } ?? ""
        if editing { return "If you tell them, Google Calendar will email the guests\(tail)." }
        return "Google Calendar will email the \(guests == 1 ? "invitation" : "invitations")\(tail)."
    }

    /// The delete question. With guests it asks about telling them, and the
    /// count is the number of people, never a guess at a list cut short.
    static func deleteWording(guests: Int, truncated: Bool) -> DeleteWording {
        let detail = "It goes from your calendar everywhere, and Passband cannot put it back."
        guard guests > 0 || truncated else {
            return DeleteWording(
                title: "Delete this event from Google Calendar?", detail: detail, tell: nil, quiet: "delete event")
        }
        let who = truncated ? "the guests" : (guests == 1 ? "1 guest" : "\(guests) guests")
        return DeleteWording(
            title: "Delete and tell \(who)?", detail: detail, tell: "delete and tell guests",
            quiet: "delete without telling")
    }

    static func deleteWording(_ event: TimelineEvent) -> DeleteWording {
        deleteWording(guests: others(event).count, truncated: event.guests_truncated == true)
    }

    static func responseWord(_ response: GuestResponse?) -> String {
        switch response {
        case .accepted: "accepted"
        case .declined: "declined"
        case .tentative: "maybe"
        case .pending, nil: "no reply"
        }
    }

    /// What to call a guest in a short line: the first word of their name,
    /// or the mailbox of their address.
    static func shortName(name: String?, email: String) -> String {
        let first = (name ?? "").split(whereSeparator: \.isWhitespace).first.map(String.init) ?? ""
        if !first.isEmpty { return first }
        return String(email.split(separator: "@").first ?? Substring(email))
    }

    /// The detail pane's list: everyone on the event, the user as "You", at
    /// most `limit` of them, and a last line for whoever is not shown.
    static func rows(_ event: TimelineEvent, limit: Int = 10) -> (rows: [GuestRow], more: String?) {
        let all = event.guests ?? []
        let rows = all.prefix(limit).enumerated().map { i, g in
            let name = (g.name ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
            return GuestRow(
                id: "\(i)|\(key(g.email))",
                name: g.isSelf == true ? "You" : (name.isEmpty ? g.email : name),
                response: responseWord(g.response), organizer: g.organizer == true)
        }
        let hidden = all.count - rows.count
        let truncated = event.guests_truncated == true
        let more: String? =
            switch (hidden > 0, truncated) {
            case (true, true): "+\(hidden) more, and others not shown"
            case (true, false): "+\(hidden) more"
            case (false, true): "more guests not shown"
            case (false, false): nil
            }
        return (rows, more)
    }

    // MARK: a guest's local time

    /// "10am ET for Sam": when the event starts where that guest is. Always
    /// with the zone. Nil when their clock reads the same as the user's at
    /// that instant: the line would only repeat the field above it. The day
    /// is said when it is not the user's ("1am Fri GMT+9 for Kenji").
    static func localTime(start: Date, zone: TimeZone, name: String, calendar: Calendar) -> String? {
        guard zone.secondsFromGMT(for: start) != calendar.timeZone.secondsFromGMT(for: start) else { return nil }
        var theirs = Calendar(identifier: .gregorian)
        theirs.timeZone = zone
        var text = Agenda.clock(start, theirs)
        let here = calendar.dateComponents([.year, .month, .day], from: start)
        let there = theirs.dateComponents([.year, .month, .day], from: start)
        if here != there { text += " " + Agenda.format(start, "EEE", theirs) }
        return "\(text) \(zoneLabel(zone, at: start)) for \(name)"
    }

    /// The zone, short: "ET" where American English has a name that short,
    /// otherwise the offset ("GMT+2", "GMT+5:30"). Fixed to American English
    /// like the rest of the app's dates, and read for the event's own date,
    /// so a March meeting is labelled for March.
    static func zoneLabel(_ zone: TimeZone, at date: Date) -> String {
        let locale = Locale(identifier: "en_US")
        if let generic = zone.localizedName(for: .shortGeneric, locale: locale), generic.count <= 4,
            !generic.contains(" "), !generic.hasPrefix("GMT")
        {
            return generic
        }
        let style: TimeZone.NameStyle = zone.isDaylightSavingTime(for: date) ? .shortDaylightSaving : .shortStandard
        if let short = zone.localizedName(for: style, locale: locale), !short.isEmpty, !short.contains(" ") {
            return short
        }
        let seconds = zone.secondsFromGMT(for: date)
        let a = abs(seconds)
        let hours = a / 3600
        let minutes = (a % 3600) / 60
        if seconds == 0 { return "GMT" }
        return "GMT\(seconds < 0 ? "-" : "+")\(hours)" + (minutes == 0 ? "" : ":\(String(format: "%02d", minutes))")
    }
}
