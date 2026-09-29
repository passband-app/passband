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
