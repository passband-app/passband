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

    /// A pulled-back or un-cancelled mail whose earlier try MAY have reached
    /// Gmail (`ScheduledCancelUndo.mayHaveReachedGmail`). Said in the composer
    /// rather than acted on: the daemon's search-before-send guarded the row,
    /// and a fresh one has no such guard, so the person looks first.
    static let mayHaveReachedGmail =
        "the last try may have reached Gmail. check Sent before sending again"

    /// A pulled-back mail took over a keyed draft row somebody else's words
    /// lived in (`PullBackDraft.arrival`). `edit` is an explicit choice, so
    /// the scheduled mail WINS and its autosave is armed at once; this says,
    /// in the past tense, what that cost.
    static func replacedDraft(forward: Bool) -> String {
        forward
            ? "replaced your unsent forward of this email"
            : "replaced your unsent draft to this thread"
    }
}

/// THE CARET'S WAY BACK after the time palette closes. The palette's field had
/// it, and a composer left with nothing focused hands the next letter typed to
/// the reader's verbs (`e` done, `h` remind, `c` compose). The pane refocuses
/// through `FocusState`; the inline reply's only field is the platform editor,
/// which takes the caret on mount alone, so it is handed a counter to watch.
enum EditorRefocus {
    /// Whether this picker change is the palette closing over `slot`'s
    /// composer: open for it before, closed now.
    static func returns<Slot: Equatable>(from old: Slot?, to new: Slot?, slot: Slot) -> Bool {
        old == slot && new == nil
    }

    /// Whether the editor owes a focus for `token`, marking it paid. The
    /// editor starts `seen` at the token it mounted with, so a mount (which
    /// has `autofocus` for that) never counts as a request, and a re-render
    /// that did not bump the token never steals the caret back.
    static func take(_ token: Int, seen: inout Int) -> Bool {
        guard token != seen else { return false }
        seen = token
        return true
    }
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

    /// THE SAME CELL, CUT IN TWO FOR A PHONE. Its row has a name line with a
    /// short stamp on the right, the way every row on that tab does, and
    /// "tomorrow 9am · in 14h" is not short: it left the recipient a third of
    /// the line. So the countdown is the stamp and the time the person picked
    /// goes under the words. Every other state is one word already and has no
    /// second half. Built from `trailing`'s own cases, so the two cannot say
    /// different things about one row.
    struct PhoneCells: Equatable {
        var stamp: Trailing
        /// "tomorrow 9am": only for a row still waiting on the time it was given.
        var when: String?
    }

    static func phoneCells(
        _ row: ScheduledSendView, now: Date = Date(), calendar: Calendar = .current
    ) -> PhoneCells {
        guard row.state != "sending", row.state != "failed", row.attempts == 0,
            let at = Fmt.date(row.send_at)
        else {
            return PhoneCells(stamp: trailing(row, now: now, calendar: calendar), when: nil)
        }
        return PhoneCells(
            stamp: Trailing(text: Fmt.relUntil(at, now: now), tone: .quiet),
            when: Fmt.sendAtChip(at, now: now, calendar: calendar))
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

    /// Line one of a NEW mail row whose `to` is empty. The daemon only files
    /// such a mail as a fan-out (its audience travels as `group_id`, which the
    /// listing does not carry) or a bcc-only send, so "no recipient" would be
    /// wrong about both. The group's name when this app scheduled it and
    /// still remembers; otherwise the truth, which is one of the two.
    static func emptyRecipient(groupName: String?) -> String {
        if let name = groupName?.trimmingCharacters(in: .whitespacesAndNewlines), !name.isEmpty {
            return name
        }
        return "group or bcc"
    }

    static let alreadySending = "already being sent"
    /// A verb on a row whose last verb is still on the wire.
    static let stillWorking = "still working on that mail"
    static let sendingNow = "sending now"
    static let noLongerScheduled = "that mail is no longer scheduled"
}

/// Which failed rows have toasted. Keyed by id AND `created_at`: outbox ids are
/// reused (no AUTOINCREMENT), and a new row failing under an old row's id is a
/// new failure somebody has to hear about. Per account: ids mean nothing
/// across daemons.
///
/// REMEMBERED ACROSS LAUNCHES (the store keeps `keys` per account in
/// UserDefaults). A failed row stays listed for 30 days, and a toast on every
/// launch and every account switch for a month is not "once", it is nagging
/// about mail the person already heard about. Bounded to the newest `cap`
/// keys, oldest dropped first: a row old enough to fall off has long been
/// swept from the listing too.
struct ScheduledFailures: Sendable, Equatable {
    static let cap = 200

    /// Toasted keys, oldest first.
    private(set) var keys: [String] = []
    private var seen: Set<String> = []

    init(keys: [String] = []) {
        let kept = Array(keys.suffix(Self.cap))
        self.keys = kept
        self.seen = Set(kept)
    }

    static func key(_ row: ScheduledSendView) -> String { "\(row.id)@\(row.created_at)" }

    /// The failed rows in `rows` that have not toasted yet, marked as toasted.
    mutating func fresh(_ rows: [ScheduledSendView]) -> [ScheduledSendView] {
        let out = rows.filter { row in
            row.state == "failed" && seen.insert(Self.key(row)).inserted
        }
        guard !out.isEmpty else { return out }
        keys.append(contentsOf: out.map(Self.key))
        if keys.count > Self.cap {
            for gone in keys.prefix(keys.count - Self.cap) { seen.remove(gone) }
            keys.removeFirst(keys.count - Self.cap)
        }
        return out
    }

    mutating func clear() { self = ScheduledFailures() }
}

/// THE GROUP NAME OF EACH FAN-OUT THIS APP SCHEDULED, by outbox id: the listing
/// carries no `group_id`, so without this a fan-out's row could only say
/// "group or bcc" (`ScheduledRowCopy.emptyRecipient`).
///
/// Outbox ids are REUSED (no AUTOINCREMENT), so a name must not outlive its
/// row: a listing that no longer carries an id drops its name (`prune`), or a
/// later row under the same id, scheduled from another device, would wear a
/// group it never had. Only names older than the listing are dropped: a POST
/// that came back while the listing was in the air named a row that listing
/// could not have seen yet. The generation is the same trick `Tombstones`
/// plays for the same reason.
struct ScheduledGroupNames: Equatable {
    private var names: [Int: (name: String, gen: Int)] = [:]
    private var next = 0

    static func == (a: Self, b: Self) -> Bool {
        a.names.mapValues(\.name) == b.names.mapValues(\.name)
    }

    subscript(id: Int) -> String? { names[id]?.name }

    /// Where a fetch starts: names set from here on survive its prune.
    var generation: Int { next }

    /// A row this app scheduled: its group's name, or none (a reused id must
    /// not keep an older row's name).
    mutating func set(_ id: Int, _ name: String?) {
        guard let name else {
            names[id] = nil
            return
        }
        names[id] = (name, next)
        next += 1
    }

    /// A pulled-back or cancelled row: its name leaves with it, and is handed
    /// back for the row an undo re-creates.
    @discardableResult
    mutating func take(_ id: Int) -> String? {
        names.removeValue(forKey: id)?.name
    }

    /// A listing that started at `since` came back with `listed`.
    mutating func prune(listed: Set<Int>, since: Int) {
        names = names.filter { listed.contains($0.key) || $0.value.gen >= since }
    }
}

/// Whether the sitrep tick asks for the outbox. Every tick while it holds
/// anything; once a listing came back EMPTY, only every `emptyEvery`: nothing
/// this app does can fill it without clearing the flag, but ANOTHER device
/// can, and a row scheduled from the phone must still toast here if it fails.
enum ScheduledPoll {
    static let emptyEvery: TimeInterval = 300

    static func due(knownEmpty: Bool, loadedAt: Date?, now: Date = Date()) -> Bool {
        guard knownEmpty, let loadedAt else { return true }
        return now.timeIntervalSince(loadedAt) >= emptyEvery
    }
}

/// THE SENT PAGE'S CURSOR through an outbox listing that changed under it. The
/// cursor is an index over the outbox rows and then the sent rows, and a poll
/// that inserts or drops an outbox row shifts every position after it: left
/// alone, the highlight (and the Enter, `s` and `x` that act on it) would land
/// on a different mail. Anchored by identity instead: an outbox row keeps its
/// row wherever it moved, a sent row keeps its place after the group.
enum ScheduledCursor {
    static func reanchor(_ index: Int, old: [Int], new: [Int]) -> Int {
        guard index >= 0 else { return index }
        if index < old.count {
            // Still listed: follow it. Gone: stay put, clamped to the group,
            // so the cursor lands on the next outbox row rather than jumping
            // into the sent mail (or past the end, when the group emptied).
            if let moved = new.firstIndex(of: old[index]) { return moved }
            return new.isEmpty ? 0 : min(index, new.count - 1)
        }
        return max(0, index + new.count - old.count)
    }

    /// Re-anchor, THEN clamp to a page of `count` rows: the one order that
    /// keeps a sent row's mail under the cursor when an outbox row above it
    /// left in the same update that shortened the page. Clamping first moves
    /// the index onto a position whose outbox identity is a different row,
    /// and the re-anchor then follows THAT row, one too high.
    static func follow(_ index: Int, old: [Int], new: [Int], count: Int) -> Int {
        max(0, min(reanchor(index, old: old, new: new), count - 1))
    }
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

    /// A row an earlier claim may have put in Gmail: the sticky `maybe_sent`,
    /// or any attempt at all. The daemon searches Gmail before it sends such
    /// a row again; a re-POST is a NEW row with no history, so it would send
    /// blind. These never re-POST from an undo: they reopen in the composer
    /// with `ScheduleCopy.mayHaveReachedGmail` for the person to check first.
    static func mayHaveReachedGmail(_ row: ScheduledSendView) -> Bool {
        row.maybe_sent == true || row.attempts > 0
    }

    /// What a composer reopened from this row says before anyone presses a
    /// key: the Gmail doubt first (it is the one that can cost a second copy),
    /// then a time that has already gone by (a failed row's, or one due while
    /// the person looked), so the chip is not a past time with nothing beside
    /// it until ⌘↵ finds out. nil when there is nothing to say.
    static func reopenError(_ row: ScheduledSendView, now: Date = Date()) -> String? {
        if mayHaveReachedGmail(row) { return ScheduleCopy.mayHaveReachedGmail }
        if let at = Fmt.date(row.send_at), at <= now { return ScheduleCopy.timePassed }
        return nil
    }
}

/// A PULLED-BACK MAIL AND THE KEYED DRAFT ROW IT LANDS ON. A reply's draft is
/// one row per parent and a forward's one per original (new mail too, on a
/// daemon without `drafts_v2`), so the reopened composer's first autosave PUTs
/// over whatever that row holds: somebody's unsent reply to the same thread.
///
/// THE SCHEDULED MAIL WINS. Pulling back is an
/// explicit `edit` from the preview now, and the cancel that reopened it has
/// already taken the mail out of the outbox: a save held back until the first
/// keystroke (the previous rule) made Esc-without-typing LOSE the scheduled
/// mail, which is the worse of the two losses. So the save is ALWAYS armed, and
/// when it will land on somebody's words the toast says so, not silently.
///
/// AND IT OPENS AT ONCE. The composer takes the mail in the same turn the
/// DELETE's answer arrives (`AppStore.openPulledBack`), before anything else is
/// awaited: from the moment the row is gone the composer is the only place the
/// mail exists, and a composer is what the account switch and ⌘Q already
/// flush. The group lookup and this drafts check run AFTER, and patch or toast
/// only a composer that is still the pulled-back one (`stillOpen`).
enum PullBackDraft {
    /// Where a pulled-back mail opens.
    enum Route: Equatable {
        /// Its thread's inline composer. `closePane`: the pane holds a reply
        /// to the SAME parent, which writes the same keyed row; it is closed
        /// (and flushed) first, so its save is on the wire before this one's
        /// and the pulled-back mail's lands last.
        case inline(threadId: String, closePane: Bool)
        /// The compose pane.
        case pane
    }

    static func route(_ state: ComposeState, threadId: String?, pane: ComposeState?) -> Route {
        guard state.replyToMessageId != nil, state.forwardOfMessageId == nil, let threadId else {
            return .pane
        }
        let rival =
            pane.map {
                $0.id != state.id && $0.forwardOfMessageId == nil
                    && $0.replyToMessageId == state.replyToMessageId
            } ?? false
        return .inline(threadId: threadId, closePane: rival)
    }

    enum Slot: Equatable { case pane, inline }

    /// Which slot still holds the pulled-back composer, for the late patch and
    /// the late toast. nil once it was sent, closed or replaced: then the
    /// group's name belongs to nobody and a "replaced" toast would describe a
    /// composer that is not on screen.
    static func stillOpen(_ id: UUID, pane: ComposeState?, inline: ComposeState?) -> Slot? {
        if pane?.id == id { return .pane }
        if inline?.id == id { return .inline }
        return nil
    }

    /// Whether the drafts list has to be asked at all: only a mail that lands
    /// on a KEYED row can replace somebody's words.
    static func keyed(_ state: ComposeState, keyedNewMail: Bool) -> Bool {
        state.replyToMessageId != nil || state.forwardOfMessageId != nil || keyedNewMail
    }

    /// The toast, or nil. `drafts` is listed AFTER the composer opened, so
    /// the pulled-back composer's own row (`ownDraftId`, once its first save
    /// has landed) is left out: it is this mail, not a rival. `open` is the
    /// composers that held words BEFORE this one took its slot.
    static func replacedToast(
        _ state: ComposeState, drafts: [DraftView], open: [ComposeState], ownDraftId: Int?,
        keyedNewMail: Bool
    ) -> String? {
        let rivals = drafts.filter { $0.id != ownDraftId }
        guard wouldReplace(state, drafts: rivals, open: open, keyedNewMail: keyedNewMail) else {
            return nil
        }
        return ScheduleCopy.replacedDraft(forward: state.forwardOfMessageId != nil)
    }

    /// `drafts`: the daemon's rows. `open`: the composers on screen that hold
    /// content (their close flush writes the same row). `keyedNewMail`: new
    /// mail shares the one nil-keyed row (an older daemon).
    static func wouldReplace(
        _ state: ComposeState, drafts: [DraftView], open: [ComposeState], keyedNewMail: Bool
    ) -> Bool {
        let original = state.forwardOfMessageId
        let parent = original == nil ? state.replyToMessageId : nil
        guard original != nil || parent != nil || keyedNewMail else { return false }
        func same(reply: Int?, forward: Int?) -> Bool {
            if let original { return forward == original }
            return forward == nil && reply == parent
        }
        return drafts.contains { same(reply: $0.reply_to_message_id, forward: $0.forward_of_message_id) }
            || open.contains { $0.id != state.id && same(reply: $0.replyToMessageId, forward: $0.forwardOfMessageId) }
    }
}

// MARK: - the read-only preview

/// WHAT THE PREVIEW OF AN OUTBOX ROW SAYS (Views/ScheduledPreview.swift):
/// opening a scheduled row SHOWS it, as scheduled, and nothing about looking at
/// it moves it. Pure for the reason the row's copy
/// is: a status line that said "scheduled" over a row that failed, or a header
/// that named the wrong thread, is the app lying about somebody's mail.
enum ScheduledPreviewCopy {
    struct Status: Equatable {
        var text: String
        var tone: ScheduledRowCopy.Tone
    }

    /// The top bar's right-hand line, one per state. The row's trailing cell
    /// plus the reason the row keeps for its second line, since the preview
    /// has one line to say both:
    ///
    /// - `scheduled`: "scheduled · Tue 9am · in 3h".
    /// - `scheduled` with a claim behind it: "retrying · <reason>".
    /// - `sending`: "sending".
    /// - `failed`: "didn't send · <reason>", EXCEPT a row that may have gone
    ///   (`ScheduledRowCopy.mayHaveSent`), which says "may have sent".
    static func status(
        _ row: ScheduledSendView, now: Date = Date(), calendar: Calendar = .current
    ) -> Status {
        switch row.state {
        case "sending":
            return Status(text: "sending", tone: .busy)
        case "failed":
            if ScheduledRowCopy.mayHaveSent(row) {
                return Status(text: "may have sent", tone: .danger)
            }
            return Status(
                text: "didn't send · \(ScheduledRowCopy.reason(row.last_error))", tone: .danger)
        default:
            if row.attempts > 0 {
                let why = row.last_error.map { " · \(ScheduledRowCopy.reason($0))" } ?? ""
                return Status(text: "retrying\(why)", tone: .busy)
            }
            guard let at = Fmt.date(row.send_at) else {
                return Status(text: "scheduled", tone: .quiet)
            }
            let chip = Fmt.sendAtChip(at, now: now, calendar: calendar)
            return Status(text: "scheduled · \(chip) · \(Fmt.relUntil(at, now: now))", tone: .quiet)
        }
    }

    struct Line: Equatable {
        var label: String
        var value: String
    }

    /// The header block, top to bottom. `cc` and `bcc` show only when the
    /// request states them: a daemon older than those fields says nothing
    /// (nil), and an empty list is not printed as "nobody else".
    ///
    /// - NEW: `to` (or the fan-out / bcc truth, as the row says it), subject.
    ///   A new mail with no `to` but a cc or bcc names nobody on the `to`
    ///   line rather than guessing "group or bcc": the lines below say who.
    /// - REPLY: `to` when stated, else the thread decides at fire time; then
    ///   "re: <parent subject> · <parent sender>", then its own subject if it
    ///   typed one (blank means the daemon titles it from the parent, and the
    ///   `re` line already says which).
    /// - FORWARD: `to`, "fwd: <original subject>", its own subject if typed.
    static func header(_ row: ScheduledSendView, groupName: String? = nil) -> [Line] {
        let reply = row.forward_of_message_id == nil && row.reply_to_message_id != nil
        let forward = row.forward_of_message_id != nil
        var lines: [Line] = []
        func list(_ raw: String?) -> String {
            SenderID.recipients(raw ?? "").joined(separator: ", ")
        }
        let to = list(row.to)
        let cc = list(row.cc)
        let bcc = list(row.bcc)
        if !to.isEmpty {
            lines.append(Line(label: "to", value: to))
        } else if reply {
            lines.append(Line(label: "to", value: replyAudience))
        } else if let name = groupName?.trimmingCharacters(in: .whitespacesAndNewlines),
            !name.isEmpty
        {
            lines.append(Line(label: "to", value: name))
        } else if !cc.isEmpty || !bcc.isEmpty {
            // Stated copies and no `to`: nothing to guess about.
            lines.append(Line(label: "to", value: nobody))
        } else {
            lines.append(Line(label: "to", value: ScheduledRowCopy.emptyRecipient(groupName: nil)))
        }
        if !cc.isEmpty { lines.append(Line(label: "cc", value: cc)) }
        if !bcc.isEmpty { lines.append(Line(label: "bcc", value: bcc)) }
        let parent = row.parent_subject?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        let parentSubject = parent.isEmpty ? noSubject : parent
        if reply {
            let from = row.parent_from.map(SenderID.readableName) ?? ""
            lines.append(
                Line(label: "re", value: from.isEmpty ? parentSubject : "\(parentSubject) · \(from)"))
        } else if forward {
            lines.append(Line(label: "fwd", value: parentSubject))
        }
        let subject = row.subject.trimmingCharacters(in: .whitespacesAndNewlines)
        if !subject.isEmpty {
            lines.append(Line(label: "subject", value: subject))
        } else if !reply && !forward {
            lines.append(Line(label: "subject", value: noSubject))
        }
        return lines
    }

    /// A reply with no stated `to`: the daemon derives its audience from the
    /// thread when it fires, so naming anyone here would be a guess.
    static let replyAudience = "the thread's people, when it sends"
    static let noSubject = "(no subject)"
    /// A `to` line with nobody on it, when the copy lines below name everyone.
    static let nobody = "(nobody)"

    /// THE BODY AS THE PREVIEW RENDERS IT: every inline picture's marker
    /// (`![name](cid:token)`, `ComposeMarkers.marker`) swapped for its file's
    /// name in brackets, `[photo.png]`. The preview has no bytes to draw
    /// (they are staged on the daemon for the send), and the reading frame
    /// DROPS an image it cannot resolve (`CidImages`), so without this a
    /// picture in the middle of the mail vanished with no trace. The name is
    /// the tray's, found by token; a token no file carries keeps its alt.
    static func placeholdingInlineImages(
        _ body: String, attachments: [OutboundAttachment]
    ) -> String {
        guard body.contains("(cid:") else { return body }
        return body.replacing(inlineMarker) { match in
            let token = String(match.2)
            let alt = String(match.1)
            let name =
                attachments.first { $0.content_id == token }?.filename
                ?? (alt.isEmpty ? "image" : alt)
            return "[\(name.filter { !"[]()".contains($0) })]"
        }
    }

    nonisolated(unsafe) private static let inlineMarker = /!\[([^\]]*)\]\(cid:([^)\s]+)\)/

    /// Under the body when the daemon could not render it: the words as typed
    /// are on screen instead, and they are not what the HTML half will look
    /// like.
    static let renderFailed = "could not render the formatting; this is the text as typed"

    enum Verb: Equatable {
        case back, edit, sendNow, cancel

        var label: String {
            switch self {
            case .back: "back"
            case .edit: "edit"
            case .sendNow: "send now"
            case .cancel: "cancel"
            }
        }
    }

    struct Key: Equatable {
        /// The KeyDispatch spelling.
        var key: String
        /// What the verb prints beside it on the Mac.
        var hint: String
        var verb: Verb
    }

    /// THE PREVIEW'S KEYS. Not `e` for edit: `e` is done everywhere else, and
    /// a done that quietly pulled a mail out of the outbox would be the worst
    /// possible misreading of it. Enter edits (it is the "open this further"
    /// key), `s` and `x` are the row's own verbs, Esc is back and leaves the
    /// mail scheduled. A `sending` row is in Gmail's hands: Esc only.
    static func keys(_ row: ScheduledSendView) -> [Key] {
        let back = Key(key: "Escape", hint: "esc", verb: .back)
        guard actionable(row) else { return [back] }
        return [
            back,
            Key(key: "Enter", hint: "↵", verb: .edit),
            Key(key: "s", hint: "s", verb: .sendNow),
            Key(key: "x", hint: "x", verb: .cancel),
            // As on the row: Backspace and Delete cancel too. Not printed;
            // `x` is the one the verb bar shows.
            Key(key: "Backspace", hint: "", verb: .cancel),
            Key(key: "Delete", hint: "", verb: .cancel),
        ]
    }

    /// The verbs at the bottom, in reading order. None for a `sending` row.
    static func verbs(_ row: ScheduledSendView) -> [Key] {
        keys(row).filter { $0.verb != .back && !$0.hint.isEmpty }
    }

    static func actionable(_ row: ScheduledSendView) -> Bool { row.state != "sending" }
}
