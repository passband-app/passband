// EVENT REMINDERS (docs/CALENDAR.md, "Reminders and tickets"): the daemon's
// banner a well-chosen time before an event, saying where it is and, when the
// mail has them, where the tickets are. This file is the pure half, shared by
// the three things that draw that banner and must draw it alike: the Mac, off
// the `calendar_reminder` frame on `/client/events`; the phone's notification
// extension, off `GET /client/calendar/reminders/{fire id}`, which serves the
// same JSON; and the test suite. Nothing here posts or fetches.
//
// The frame is like `renotify`: no `id:`, so it moves no cursor, and no
// `Event` shape, so it is decoded only after a frame failed to decode as one.
// Its identity is the FIRE (one per start instant), never an event of the mail
// feed, so its userInfo carries no `eventKey` and `ToldEvents` never swallows
// it. The tap half (userInfo keys, `CalendarReminderTap`, the push id) lives
// in EventBanner.swift, because `tapRoute` reads it.
//
// The `line` is model text about somebody's mail, laundered by the daemon
// (no links, no addresses, no long digit runs). It is still flattened and
// stripped of bidi controls here, as every banner's words are.

import Foundation

/// The `calendar_reminder` frame, and the body of
/// `GET /client/calendar/reminders/{fire id}`.
struct CalendarReminderSignal: Decodable, Equatable, Sendable {
    /// What the reminder is about. A ghost's `id` is a number on the wire
    /// and an event's a string; both are kept as a string.
    struct Item: Decodable, Equatable, Sendable {
        enum Kind: String, LenientRawEnum {
            case event, ghost, other
            static var unknownFallback: Self { .other }
        }

        var kind: Kind
        var calendar_id: String?
        var id: String
        var title: String?
        var start: String
        var end: String?
        var all_day: Bool
        /// The card's place (its name), else the raw location.
        var place: String?

        init(
            kind: Kind, calendar_id: String? = nil, id: String, title: String?, start: String,
            end: String? = nil, all_day: Bool = false, place: String? = nil
        ) {
            self.kind = kind
            self.calendar_id = calendar_id
            self.id = id
            self.title = title
            self.start = start
            self.end = end
            self.all_day = all_day
            self.place = place
        }

        init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            kind = (try? c.decode(Kind.self, forKey: .kind)) ?? .other
            calendar_id = try? c.decodeIfPresent(String.self, forKey: .calendar_id)
            if let text = try? c.decode(String.self, forKey: .id) {
                id = text
            } else {
                id = String(try c.decode(Int.self, forKey: .id))
            }
            title = try? c.decodeIfPresent(String.self, forKey: .title)
            start = try c.decode(String.self, forKey: .start)
            end = try? c.decodeIfPresent(String.self, forKey: .end)
            all_day = (try? c.decodeIfPresent(Bool.self, forKey: .all_day)) ?? false
            place = try? c.decodeIfPresent(String.self, forKey: .place)
        }

        private enum CodingKeys: String, CodingKey {
            case kind, calendar_id, id, title, start, end, all_day, place
        }

        /// The Calendar page's row id for it (`Agenda.itemId`, and the
        /// ghost's `g:`), nil when the page could not have such a row.
        var agendaId: String? {
            switch kind {
            case .ghost: return id.isEmpty ? nil : "g:\(id)"
            case .event:
                guard let calendar_id, !calendar_id.isEmpty, !id.isEmpty else { return nil }
                return "e:\(calendar_id)|\(id)"
            case .other: return nil
            }
        }
    }

    /// `open` or `ticket`: a message, its thread, and maybe one attachment.
    /// `ticket` also names the attachment's file; `open` may not.
    struct Target: Decodable, Equatable, Sendable {
        var message_id: Int
        var thread_id: String
        var attachment_id: Int?
        var filename: String?
        var mime: String?

        init(
            message_id: Int, thread_id: String, attachment_id: Int? = nil, filename: String? = nil,
            mime: String? = nil
        ) {
            self.message_id = message_id
            self.thread_id = thread_id
            self.attachment_id = attachment_id
            self.filename = filename
            self.mime = mime
        }

        init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            message_id = try c.decode(Int.self, forKey: .message_id)
            thread_id = (try? c.decodeIfPresent(String.self, forKey: .thread_id)) ?? ""
            attachment_id = try? c.decodeIfPresent(Int.self, forKey: .attachment_id)
            filename = try? c.decodeIfPresent(String.self, forKey: .filename)
            mime = try? c.decodeIfPresent(String.self, forKey: .mime)
        }

        private enum CodingKeys: String, CodingKey {
            case message_id, thread_id, attachment_id, filename, mime
        }

        var file: EventBanner.CalendarFile {
            EventBanner.CalendarFile(
                messageId: message_id, threadId: thread_id,
                attachmentId: attachment_id.flatMap { $0 > 0 ? $0 : nil }, filename: filename, mime: mime)
        }
    }

    /// An entry code the event's own text gives ("Door PIN 735902"), held to
    /// that text by the daemon (`remind::settle_code`).
    struct EntryCode: Decodable, Equatable {
        var label: String
        var value: String
    }

    /// The fire id.
    let calendar_reminder: Int
    var at: Int?
    var item: Item
    var lead_minutes: Int?
    var line: String?
    var open: Target?
    var ticket: Target?
    var code: EntryCode?

    init(
        calendar_reminder: Int, at: Int? = nil, item: Item, lead_minutes: Int? = nil,
        line: String? = nil, open: Target? = nil, ticket: Target? = nil, code: EntryCode? = nil
    ) {
        self.calendar_reminder = calendar_reminder
        self.at = at
        self.item = item
        self.lead_minutes = lead_minutes
        self.line = line
        self.open = open
        self.ticket = ticket
        self.code = code
    }

    /// LENIENT EVERYWHERE BUT THE SPINE. The fire id, the item's id and its
    /// start are what make it a reminder; a `line`, a `lead_minutes`, an
    /// `open` or a `ticket` of the wrong shape costs only itself, so a newer
    /// daemon's change to one of them still puts the banner up.
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        calendar_reminder = try c.decode(Int.self, forKey: .calendar_reminder)
        at = try? c.decodeIfPresent(Int.self, forKey: .at)
        item = try c.decode(Item.self, forKey: .item)
        lead_minutes = try? c.decodeIfPresent(Int.self, forKey: .lead_minutes)
        line = try? c.decodeIfPresent(String.self, forKey: .line)
        open = try? c.decodeIfPresent(Target.self, forKey: .open)
        ticket = try? c.decodeIfPresent(Target.self, forKey: .ticket)
        code = try? c.decodeIfPresent(EntryCode.self, forKey: .code)
    }

    private enum CodingKeys: String, CodingKey {
        case calendar_reminder, at, item, lead_minutes, line, open, ticket, code
    }

    /// Where one fired reminder is served, for the app and the extension.
    static func path(_ fireId: Int) -> String { CalendarReminderRoute.fire(fireId) }

    static func parse(_ data: String) -> CalendarReminderSignal? {
        guard let bytes = data.data(using: .utf8) else { return nil }
        return decode(bytes)
    }

    static func decode(_ bytes: Data) -> CalendarReminderSignal? {
        guard let signal = try? JSONDecoder().decode(CalendarReminderSignal.self, from: bytes),
            signal.calendar_reminder > 0, !signal.item.start.isEmpty
        else { return nil }
        return signal
    }

    /// `renotify`'s rule (`AgentOpenSignal.freshFor`, restated here because
    /// the notification extension compiles this file without that one): a
    /// frame a sleeping Mac receives when the lid opens is about a moment
    /// that has passed. A frame with no date is taken as fresh.
    static let freshFor = 60

    func isFresh(now: Int) -> Bool {
        guard let at else { return true }
        return abs(now - at) <= Self.freshFor
    }
}

extension EventBanner {
    /// What a reminder banner says. A subtitle, which no mail banner has:
    /// when and where, apart from the model's line.
    struct ReminderCopy: Equatable {
        var title: String
        var subtitle: String
        var body: String
        var threadIdentifier: String
        var sound: Bool
    }

    /// THE SAME FIRE: the reminder a push named, by id AND by the second it
    /// fired (`at`, which the daemon sets to the push's `fired`). An id alone
    /// is not enough: a tap long after on an old notification must never
    /// open a different reminder.
    static func isPushedFire(_ signal: CalendarReminderSignal, fireId: Int, fired: Int) -> Bool {
        signal.calendar_reminder == fireId && signal.at == fired
    }

    /// `stripBidi`, and the invisible marks it leaves: the directional marks
    /// (LRM, RLM, ALM), zero-width space and non-joiner, word joiner and
    /// invisible operators, the BOM and tag characters. The daemon strips
    /// them at sync; a reminder's words are stripped again here, as the
    /// Mac and the phone draw them. The zero-width JOINER stays: emoji are
    /// made with it.
    static func stripInvisible(_ s: String) -> String {
        String(
            String.UnicodeScalarView(
                stripBidi(s).unicodeScalars.filter { scalar in
                    switch scalar.value {
                    case 0x200B, 0x200C, 0x200E, 0x200F, 0x061C, 0x2060...0x2064, 0xFEFF,
                        0xE0000...0xE007F:
                        return false
                    default: return true
                    }
                }))
    }

    /// The relay's alert, which is also the extension's fallback when the
    /// daemon cannot be asked.
    static let calendarFallbackBody = "An event is coming up"

    /// The request identifier, NAMESPACED BY ACCOUNT: fire ids are one
    /// daemon's ints.
    static func calendarReminderIdentifier(account: UUID, fire: Int) -> String {
        "passband.calendar.\(account.uuidString).\(fire)"
    }

    /// The banner: title = the event's title, subtitle = "8pm · Riverside
    /// Hall", body = the line (nothing when there is none).
    ///
    /// THE CLOCK TIME, NOT "in 1 hr": a banner sits on the lock screen, and
    /// a countdown read forty minutes later is wrong while a start time never
    /// goes stale. The countdown is only the fallback for a start that cannot
    /// be read.
    static func calendarReminderCopy(
        _ signal: CalendarReminderSignal, now: Date = Date(), zone: TimeZone = .current
    ) -> ReminderCopy {
        let title = flatten(stripInvisible(signal.item.title ?? ""), max: 80)
        let lead =
            reminderClock(start: signal.item.start, allDay: signal.item.all_day, zone: zone)
            ?? reminderLead(
                start: signal.item.start, allDay: signal.item.all_day, leadMinutes: signal.lead_minutes,
                now: now)
        let place = flatten(stripInvisible(signal.item.place ?? ""), max: 60)
        let subtitle = [lead, place.isEmpty ? nil : place].compactMap { $0 }.joined(separator: " · ")
        return ReminderCopy(
            title: title.isEmpty ? "Untitled event" : title,
            subtitle: subtitle,
            body: reminderBody(signal),
            threadIdentifier: calendarGroup,
            // Time-bound by definition: it chimes.
            sound: true)
    }

    /// The body: the entry code first, where a glance finds it ("Door PIN
    /// 735902"), then the line. Either may be missing.
    static func reminderBody(_ signal: CalendarReminderSignal) -> String {
        let line = flatten(stripInvisible(signal.line ?? ""), max: 120)
        var code = ""
        if let entry = signal.code {
            let value = flatten(stripInvisible(entry.value), max: 12)
            let label = flatten(stripInvisible(entry.label), max: 24)
            if !value.isEmpty { code = label.isEmpty ? value : "\(label) \(value)" }
        }
        return [code, line].filter { !$0.isEmpty }.joined(separator: " · ")
    }

    /// "8pm", "8:30pm", or "Today" for an all-day event; nil when the start
    /// cannot be read.
    static func reminderClock(start: String, allDay: Bool, zone: TimeZone) -> String? {
        if allDay { return "Today" }
        guard let date = eventDate(start) else { return nil }
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = zone
        let parts = calendar.dateComponents([.hour, .minute], from: date)
        guard let hour = parts.hour, let minute = parts.minute else { return nil }
        let twelve = hour % 12 == 0 ? 12 : hour % 12
        let half = hour < 12 ? "am" : "pm"
        return minute == 0 ? "\(twelve)\(half)" : String(format: "%d:%02d%@", twelve, minute, half)
    }

    /// "in 10 min", "in 1 hr", "in 1 hr 30 min", "Starting now", or "Today"
    /// for an all-day event (its reminder comes at 08:00 on the day).
    ///
    /// FROM THE CLOCK, NOT FROM `lead_minutes`: a reminder that fires late
    /// (a daemon that was down, a plan that landed late) fires at once, and
    /// "in 1 hr" twelve minutes before the doors would be a lie. The lead is
    /// the fallback when the start cannot be read. Rounded to the minute, so
    /// a 60-minute reminder posted a few seconds late still says "in 1 hr".
    static func reminderLead(start: String, allDay: Bool, leadMinutes: Int?, now: Date) -> String? {
        if allDay { return "Today" }
        let minutes: Int
        if let date = eventDate(start) {
            minutes = Int((date.timeIntervalSince(now) / 60).rounded())
        } else if let leadMinutes {
            minutes = leadMinutes
        } else {
            return nil
        }
        if minutes <= 0 { return "Starting now" }
        if minutes < 60 { return "in \(minutes) min" }
        let hours = minutes / 60
        let rest = minutes % 60
        return rest == 0 ? "in \(hours) hr" : "in \(hours) hr \(rest) min"
    }

    /// What a tap on this reminder needs, written into the banner so it
    /// works after a relaunch with nothing fetched.
    static func calendarReminderUserInfo(_ signal: CalendarReminderSignal, account: UUID)
        -> [AnyHashable: Any]
    {
        var info: [AnyHashable: Any] = [
            routeKey: calendarRoute,
            accountKey: account.uuidString,
            calendarFireKey: signal.calendar_reminder,
            calendarStartKey: signal.item.start,
        ]
        if let id = signal.item.agendaId { info[calendarItemKey] = id }
        if let open = signal.open {
            for (k, v) in calendarFileInfo(open.file, calendarOpenPrefix) { info[k] = v }
        }
        if let ticket = signal.ticket {
            for (k, v) in calendarFileInfo(ticket.file, calendarTicketPrefix) { info[k] = v }
        }
        return info
    }

    /// The tap a reminder carries, straight from the frame: what the app
    /// routes when the extension could not dress the push and the app had to
    /// fetch the reminder itself.
    static func calendarReminderTap(_ signal: CalendarReminderSignal, account: UUID)
        -> CalendarReminderTap
    {
        CalendarReminderTap(
            accountId: account, fireId: signal.calendar_reminder, itemId: signal.item.agendaId,
            start: signal.item.start, open: signal.open?.file, ticket: signal.ticket?.file)
    }
}
