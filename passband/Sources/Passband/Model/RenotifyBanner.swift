// SMART RE-NOTIFY on the Mac (issue #625, docs/RENOTIFY.md): the daemon's
// second banner for mail that still needs handling, sent at a moment the owner
// can act on it. This file is the pure half, so the test suite compiles it
// alone: the frame, whether it is fresh, what the banner says, and where a tap
// on it goes. `Notifier.postRenotify` posts it; `EventStream` hands it over.
//
// The frame rides the Mac stream (`?opens=1`) like `open_draft`: no `id:`, so
// it moves no cursor, and no `Event` shape, so it is decoded only after a
// frame failed to decode as one. It is NOT an event and must not be deduped as
// one: its identity is the BATCH, and its userInfo carries no `eventKey`, so
// `ToldEvents` never swallows it. The tap half (its userInfo keys and
// `renotifyTap`) lives in EventBanner.swift, because `tapRoute` reads it and
// the phone's notification extension compiles that file without this one. The backlog rule never sees it either: that
// rule reads an event's `created_at`, and a stale frame is dropped here first.

import Foundation

/// The `renotify` frame:
/// `{"renotify": <batch>, "trigger": "...", "at": <unix secs>, "items": [...]}`.
struct RenotifySignal: Decodable, Equatable, Sendable {
    struct Item: Decodable, Equatable, Sendable {
        /// The `events` row it first notified on; nil for deadline mail whose
        /// first banner never happened.
        var event_id: Int?
        var message_id: Int
        var thread_id: String
        var kind: EventKind
        var importance: Int
        var sender: String
        var one_line: String
        /// The deadline snapshot (RFC 3339), display copy only.
        var deadline: String?
        /// Why it is back. Absent from the daemon's first cut of the frame,
        /// which only ever re-notified unanswered mail.
        var why: Why?
        /// For `due_soon`: the due time the daemon judged (RFC 3339). It is
        /// what "Due tomorrow" is worded from.
        var due_at: String?

        var isDueSoon: Bool { why == .due_soon }
        /// The date a due-soon banner names: the judged due time, else the
        /// snapshot.
        var dueDate: String? { due_at ?? deadline }
    }

    /// Why one message is back. Lenient: a newer daemon's reason reads as the
    /// plain "still waiting" framing rather than failing the frame.
    enum Why: String, LenientRawEnum {
        case unhandled
        case due_soon
        static var unknownFallback: Self { .unhandled }
    }

    let renotify: Int
    let trigger: String
    var at: Int?
    let items: [Item]

    /// The trigger, when it is one this app knows. Only this reaches
    /// analytics; the frame's own string stays a String so an unknown one
    /// still shows its banner.
    var knownTrigger: EventBanner.RenotifyTrigger? { EventBanner.RenotifyTrigger(rawValue: trigger) }

    static func parse(_ data: String) -> RenotifySignal? {
        guard let bytes = data.data(using: .utf8),
            let signal = try? JSONDecoder().decode(RenotifySignal.self, from: bytes),
            signal.renotify > 0, !signal.items.isEmpty,
            signal.items.allSatisfy({ $0.message_id > 0 })
        else { return nil }
        return signal
    }

    /// THE SAME RULE AS `open_draft` (`AgentOpenSignal.freshFor`): a frame a
    /// sleeping Mac receives when the lid opens is a banner about a moment
    /// that has passed. The daemon chose that moment on purpose (just after a
    /// meeting, just after sitting down), so a late one is dropped, never
    /// shown. A frame with no date is taken as fresh.
    func isFresh(now: Int) -> Bool {
        guard let at else { return true }
        return abs(now - at) <= AgentOpenSignal.freshFor
    }
}

extension EventBanner {
    /// The request identifier, NAMESPACED BY ACCOUNT like an event's: batch
    /// ids are one daemon's SQLite ints. Never "passband.event.…", so it can
    /// neither replace nor be replaced by the first banner for the same mail.
    static func renotifyIdentifier(account: UUID, batch: Int) -> String {
        "passband.renotify.\(account.uuidString).\(batch)"
    }

    /// The userInfo a re-notify banner carries. No `eventKey`: that is what
    /// `eventIdentity` (and so `ToldEvents`) reads, and this banner is about a
    /// batch, not about an event the reader was already told of.
    static func renotifyUserInfo(_ signal: RenotifySignal, account: UUID) -> [AnyHashable: Any] {
        let first = signal.items[0]
        return [
            routeKey: renotifyRoute,
            renotifyBatchKey: signal.renotify,
            renotifyTriggerKey: signal.trigger,
            renotifyCountKey: signal.items.count,
            messageKey: first.message_id,
            threadKey: first.thread_id,
            accountKey: account.uuidString,
        ]
    }

    /// What a re-notify banner says.
    ///
    /// ONE MESSAGE reads like its first banner (`copy(for:)`: the sender as
    /// the title, the summary as the body) framed as a reminder: "Still
    /// waiting: …", or for due-soon mail, "Due tomorrow: …". The date is the
    /// due time the daemon judged (`due_at`), not the first banner's
    /// snapshot, which is why this banner may say it when the first one
    /// deliberately did not.
    ///
    /// SEVERAL are one banner, "3 emails still need you", naming a couple of
    /// senders. A deadline among them leads the body.
    ///
    /// A chime only when a deadline is the reason: a reminder of mail that is
    /// merely waiting is a nudge, not an alarm.
    static func renotifyCopy(
        for signal: RenotifySignal, now: Date = Date(), calendar: Calendar = .current
    ) -> Copy {
        let items = signal.items
        let anyDeadline = items.contains(where: \.isDueSoon)
        if items.count == 1 {
            let item = items[0]
            let original = copy(for: event(from: item))
            let lead =
                item.isDueSoon
                ? dueLabel(item.dueDate, now: now, calendar: calendar) ?? "Due soon"
                : "Still waiting"
            let summary = flatten(item.one_line, max: 220)
            return Copy(
                title: original.title,
                body: summary.isEmpty ? "\(lead)." : "\(lead): \(summary)",
                threadIdentifier: renotifyGroup,
                sound: anyDeadline)
        }
        var names: [String] = []
        for item in items {
            let name = flatten(SenderID.readableName(item.sender), max: 40)
            if !name.isEmpty, !names.contains(name) { names.append(name) }
        }
        let shown = Array(names.prefix(2))
        let others = items.count - shown.count
        var who: String
        switch (shown.count, others) {
        case (0, _): who = ""
        case (1, 0): who = "From \(shown[0])"
        case (2, 0): who = "From \(shown[0]) and \(shown[1])"
        case (_, let n): who = "From \(shown.joined(separator: ", ")) and \(n) more"
        }
        if let due = items.first(where: \.isDueSoon),
            let label = dueLabel(due.dueDate, now: now, calendar: calendar)
        {
            let dueWho = flatten(SenderID.readableName(due.sender), max: 40)
            let head = dueWho.isEmpty ? label : "\(label): \(dueWho)"
            who = who.isEmpty ? head : "\(head). \(who)"
        }
        return Copy(
            title: "\(items.count) emails still need you",
            body: who.isEmpty ? "Mail that is still waiting on you." : who,
            threadIdentifier: renotifyGroup,
            sound: anyDeadline)
    }

    /// "Due today", "Due tomorrow", "Due Friday" (within the week), "Due Nov
    /// 3", or "Past due", from an RFC 3339 deadline in the reader's calendar.
    /// nil when there is no readable date.
    static func dueLabel(_ deadline: String?, now: Date, calendar: Calendar) -> String? {
        guard let deadline, let due = eventDate(deadline) else { return nil }
        if due < now { return "Past due" }
        let days =
            calendar.dateComponents(
                [.day], from: calendar.startOfDay(for: now), to: calendar.startOfDay(for: due)
            ).day ?? 0
        switch days {
        case 0: return "Due today"
        case 1: return "Due tomorrow"
        case 2...6:
            let f = DateFormatter()
            f.calendar = calendar
            f.timeZone = calendar.timeZone
            f.locale = Locale(identifier: "en_US_POSIX")
            f.dateFormat = "EEEE"
            return "Due \(f.string(from: due))"
        default:
            let f = DateFormatter()
            f.calendar = calendar
            f.timeZone = calendar.timeZone
            f.locale = Locale(identifier: "en_US_POSIX")
            f.dateFormat = "MMM d"
            return "Due \(f.string(from: due))"
        }
    }

    /// The item as the event its first banner was built from, so the single
    /// banner's title is worded by the one function that words every banner.
    private static func event(from item: RenotifySignal.Item) -> Event {
        Event(
            id: item.event_id ?? 0, kind: item.kind, message_id: item.message_id,
            thread_id: item.thread_id, tier: .signal, importance: item.importance,
            sender: item.sender, one_line: item.one_line, deadline: item.deadline,
            created_at: "", sealed_kind: nil, is_auth: false)
    }
}
