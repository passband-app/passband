// SCHEDULED SEND, the pure half of the composer's side: the words the send
// verb flips to, the toast, and what a failed undo means. The palette and the
// clock are Views/SchedulePalette.swift; the request is `SendBody(compose:)`
// plus a time (Model/ComposeState.swift, Model/APIClient.swift); the store's
// half is `AppStore.scheduleCompose`.
//
// Kept pure so test.sh holds the verbs to their one computed string: the
// button, its key hint and the phone's button all read from here, and a label
// that said "send" over a click that schedules (or the reverse) would be the
// composer lying about the one irreversible act in the app.

import Foundation

enum ScheduleCopy {
    /// The primary verb. ONE string for the button, the Mac's key hint and the
    /// phone's button: "schedule" the moment a time is set, because the button
    /// says what it does.
    static func sendVerb(scheduled: Bool) -> String {
        scheduled ? "schedule" : "send"
    }

    /// The override, past a guard verdict. Same flip, same reason.
    static func overrideVerb(scheduled: Bool) -> String {
        scheduled ? "schedule anyway" : "send anyway"
    }

    /// The set clock's label and the toast's time: `Fmt.sendAtChip`.
    static func chip(_ sendAt: Date, now: Date = Date(), calendar: Calendar = .current)
        -> String
    {
        Fmt.sendAtChip(sendAt, now: now, calendar: calendar)
    }

    /// The toast after the outbox took it. Doubles as the undo's label, so an
    /// undo reads "undone: scheduled for Tue 9am".
    static func scheduledToast(
        _ sendAt: Date, now: Date = Date(), calendar: Calendar = .current
    ) -> String {
        "scheduled for \(chip(sendAt, now: now, calendar: calendar))"
    }

    /// Refused before the composer closes: a time that has gone by while the
    /// composer sat open. The daemon would 400 it; saying so here keeps the
    /// mail on screen instead of bouncing it through a failed request.
    static let timePassed = "that time has passed. pick a new one"

    /// The clock's help text, and the chord it prints on the Mac.
    static let clockHelp = "send later"
}

/// What a failed undo of a schedule means. The DELETE is the undo; a 409 is
/// the daemon saying the row is already in Gmail's hands, which is not a
/// failure to reach anything but a mail that went.
enum ScheduleUndo {
    enum Verdict: Equatable {
        /// 409: the worker already claimed it. Nothing to reopen.
        case alreadySent
        /// Anything else: it may well still be scheduled, and the Sent page
        /// can still cancel it.
        case failed
    }

    static func verdict(_ error: Error) -> Verdict {
        (error as? APIError)?.status == 409 ? .alreadySent : .failed
    }

    /// The toast when the undo lost the race.
    static let alreadySentToast = "undo failed: already sent"
}

// MARK: - the Sent page's outbox rows

/// What an outbox row SAYS on the Sent page, kept pure for the reason the
/// verbs above are: "in 3h" over a row that already failed, or "didn't send"
/// over one that may well have gone, is the app lying about somebody's mail.
/// The row (Views/ScheduledRow.swift) only picks colours for these.
enum ScheduledRowCopy {
    enum Tone: Equatable {
        /// Waiting for its time: the chip is a fact, not a warning.
        case quiet
        /// Something is happening to it now (sending, a retry pending).
        case busy
        /// It did not go, and it will not unless somebody acts.
        case danger
    }

    struct Trailing: Equatable {
        var text: String
        var tone: Tone
    }

    /// The row's right-hand cell, one per state:
    ///
    /// - `scheduled`: "tomorrow 9am · in 14h", the chip the composer showed
    ///   plus how long is left.
    /// - `scheduled` with a claim behind it (`attempts > 0`): "retrying". Its
    ///   time is the backoff's, not the one the person picked, so showing it
    ///   as theirs would be wrong.
    /// - `sending`: "sending". In Gmail's hands; nothing here can stop it.
    /// - `failed`: "didn't send", EXCEPT a row that MAY have gone out
    ///   (`mayHaveSent`): that row says "may have sent", and saying "didn't
    ///   send" over it would invite a second copy.
    static func trailing(
        _ row: ScheduledSendView, now: Date = Date(), calendar: Calendar = .current
    ) -> Trailing {
        switch row.state {
        case "sending":
            return Trailing(text: "sending", tone: .busy)
        case "failed":
            return Trailing(
                text: mayHaveSent(row) ? "may have sent" : "didn't send", tone: .danger)
        default:
            if row.attempts > 0 { return Trailing(text: "retrying", tone: .busy) }
            guard let at = Fmt.date(row.send_at) else {
                return Trailing(text: "scheduled", tone: .quiet)
            }
            let chip = Fmt.sendAtChip(at, now: now, calendar: calendar)
            return Trailing(text: "\(chip) · \(Fmt.relUntil(at, now: now))", tone: .quiet)
        }
    }

    /// The row's second line, when the state has something to say that the
    /// subject and snippet do not: WHY a failed row did not go, and why a
    /// retrying one is waiting. nil for everything else.
    static func reasonLine(_ row: ScheduledSendView) -> String? {
        switch row.state {
        case "failed":
            return mayHaveSent(row) ? mayHaveSentLine : reason(row.last_error)
        case "scheduled" where row.attempts > 0 || row.last_error != nil:
            return row.last_error.map(reason)
        default:
            return nil
        }
    }

    /// A failed row an earlier attempt MAY have put in Gmail: the daemon's
    /// sticky `maybe_sent` (an ambiguous post, a crash loop, a recovery that
    /// never got to ask), whatever the reason code says; `failed:recovery`
    /// also counts for a daemon older than the flag (docs/SCHEDULED-SEND.md
    /// §4). A send-now on it searches Gmail before it sends.
    static func mayHaveSent(_ row: ScheduledSendView) -> Bool {
        row.state == "failed"
            && (row.maybe_sent == true || row.last_error == "failed:recovery")
    }

    /// What a `mayHaveSent` row says on line two and in its toast, in place
    /// of the reason: the reason ("Gmail refused it") would read as "it did
    /// not go", which is the one thing nobody knows.
    static let mayHaveSentLine = "may have been sent, check Sent"

    /// The daemon's redacted reason (`failed:gmail`, `rejected:too_large`, ...)
    /// as a person would say it. A CLOSED MAP of the codes the send path and
    /// the worker write; anything else is "something went wrong", never the
    /// raw code, which reads as a crash.
    static func reason(_ code: String?) -> String {
        switch code {
        case "failed:recovery": mayHaveSentLine
        case "failed:gmail": "Gmail refused it"
        case "rejected:no_write_credential": "sending is not connected, reconnect your account"
        case "recovery:unreachable": "could not reach Gmail"
        case "recovery:grace": "waiting to confirm the last try"
        case "failed:search": "could not check with Gmail"
        case "paced": "Gmail is pacing sends, waiting a moment"
        case "failed:crash_loop": "sending it kept failing"
        case "failed:request": "the saved mail could not be read"
        case "failed:attachment_missing": "a file it held is gone"
        case "failed:fetch_original": "the email it forwards is gone"
        case "failed:target": "the email it answers is gone"
        case "failed:group", "rejected:unresolved_group", "failed:group_fanout":
            "its group is gone"
        case "rejected:too_large": "it is too large to send"
        case "rejected:no_recipient": "it has no recipient"
        case "rejected:empty_body": "it has no message"
        case "failed:store": "a local storage error"
        default: "something went wrong"
        }
    }

    /// The toast a newly failed row raises, once per app run
    /// (`ScheduledFailures`). A `mayHaveSent` row is not "didn't send": it
    /// might have, and the toast says to look before sending again.
    static func failedToast(_ row: ScheduledSendView) -> String {
        mayHaveSent(row) ? mayHaveSentLine : "didn't send: \(reason(row.last_error))"
    }

    /// The Sent page's order: the next to go on top, oldest `send_at` first,
    /// id breaking ties. The daemon lists in this order already; sorting here
    /// as well keeps a row the store re-inserts (a failed cancel) from landing
    /// at the end.
    static func ordered(_ rows: [ScheduledSendView]) -> [ScheduledSendView] {
        let keyed: [(row: ScheduledSendView, at: Date)] = rows.map { row in
            (row, Fmt.date(row.send_at) ?? .distantFuture)
        }
        let sorted = keyed.sorted { a, b in
            a.at != b.at ? a.at < b.at : a.row.id < b.row.id
        }
        return sorted.map(\.row)
    }

    /// The cancel's undo label: "cancelled: Lunch plans". Names the mail for
    /// the same reason a draft delete's does.
    static func cancelledLabel(_ row: ScheduledSendView) -> String {
        let subject = row.subject.trimmingCharacters(in: .whitespacesAndNewlines)
        if !subject.isEmpty { return "cancelled: \(Fmt.truncate(subject, 40))" }
        if let parent = row.parent_subject?.trimmingCharacters(in: .whitespacesAndNewlines),
            !parent.isEmpty
        {
            return "cancelled: re \(Fmt.truncate(parent, 40))"
        }
        return "cancelled scheduled mail"
    }

    static let alreadySending = "already being sent"
    static let sendingNow = "sending now"
    static let noLongerScheduled = "that mail is no longer scheduled"
}

/// Which failed rows have toasted this app run. Keyed by id AND `created_at`:
/// outbox ids are reused (no AUTOINCREMENT), and a new row failing under an old
/// row's id is a new failure somebody has to hear about. Per account: the
/// store clears it with the account, because ids mean nothing across daemons.
struct ScheduledFailures: Sendable, Equatable {
    private var seen: Set<String> = []

    /// The failed rows in `rows` that have not toasted yet, marked as toasted.
    mutating func fresh(_ rows: [ScheduledSendView]) -> [ScheduledSendView] {
        rows.filter { row in
            row.state == "failed" && seen.insert("\(row.id)@\(row.created_at)").inserted
        }
    }

    mutating func clear() { seen = [] }
}

/// The undo of a cancel from the Sent page: put the mail back in the outbox.
enum ScheduledCancelUndo {
    /// The re-POST's body, from the request the DELETE handed back. The
    /// daemon strips `override_guard` and `confirm` from that request on
    /// purpose: `confirm` is the client's to set, and a reschedule is judged
    /// by the outbound guard AGAIN (a verdict does not survive a cancel).
    /// No `draft_id`: scheduling consumed the draft long ago.
    static func body(_ r: ScheduledRequest) -> SendBody {
        SendBody(
            reply_to_message_id: r.reply_to_message_id,
            forward_of_message_id: r.forward_of_message_id,
            to: r.to, cc: r.cc, bcc: r.bcc, group_id: r.group_id,
            subject: r.subject, body: r.body ?? "", body_format: r.body_format,
            confirm: true, override_guard: nil, draft_id: nil,
            include_tracker: r.include_tracker, reply_all: r.reply_all,
            attachment_ids: r.attachment_ids)
    }

    /// Whether the undo may simply re-POST at the original time. Not when that
    /// time is already here (a failed row, or one due inside the undo
    /// window): the daemon refuses a past `send_at`, and moving it to "now"
    /// would make an UNDO send mail sooner than anyone asked, the one thing an
    /// undo must never do. Those reopen in the composer instead, time shown,
    /// for the person to decide. `margin` covers the POST's own trip.
    static func canReschedule(at sendAt: Date, now: Date = Date(), margin: TimeInterval = 15)
        -> Bool
    {
        sendAt.timeIntervalSince(now) > margin
    }
}
