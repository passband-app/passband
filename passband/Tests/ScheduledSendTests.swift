// The composer's half of scheduled send, against the wire the daemon answers
// with. Every failure here is invisible on screen until it is somebody's mail:
// a verb that says "send" over a click that schedules, a draft that quietly
// saves a send time and replays it on restore, a schedule body that drifts
// from the send body it is supposed to BE, an undo that reopens a composer
// missing its Bcc or its files.

import Foundation

@main
@MainActor
struct ScheduledSendTests {
    static var failures = 0
    static var checks = 0

    static func main() {
        theVerbSaysWhatTheButtonDoes()
        aDraftNeverSavesTheSendTime()
        theScheduleBodyIsTheSendBodyPlusWhen()
        aScheduleNamesTheDraftItConsumes()
        undoReopensTheComposerFromTheCancel()
        undoLendsDisplayStateButNeverIdentity()
        aCancelWhoseRequestNoLongerParsesStillDecodes()
        aConflictIsAlreadySent()
        theChipAndTheToastReadAsAWallClock()
        theListingDecodes()
        // The Sent page's half.
        eachRowStateSaysWhatItIs()
        thePhoneSplitsOnlyTheWaitingRow()
        aFailedRowSaysWhy()
        relUntilCountsDown()
        theOutboxSitsInFireOrder()
        aFailureToastsOncePerRow()
        aCancelsUndoRebuildsTheSchedule()
        anUndoNeverSendsSoonerThanAsked()
        aTombstoneOutlivesOnlyTheFetchesThatCouldCarryIt()
        aTombstoneWaitsForTheOldestFetchInTheAir()
        // Review round four.
        theEditorTakesTheCaretOncePerRequest()
        aFanOutReopensWithItsAudience()
        aPullBackNeverSilentlyReplacesAKeyedDraft()
        aFailureToastsOnceAcrossLaunches()
        anEmptyOutboxIsStillAskedNowAndThen()
        theCursorFollowsItsRow()
        aRowGmailMayHaveIsNeverResentBlind()
        aFanOutRowSaysWhoItIsFor()
        // The read-only preview.
        anEditArmsItsSaveAndSaysWhatItReplaced()
        aPullBackOpensWhereItBelongs()
        aLateAnswerLandsOnlyOnItsOwnComposer()
        aFanOutNameLeavesWithItsRow()
        anInlinePictureIsNamedNotDropped()
        thePreviewSaysWhereTheMailIs()
        thePreviewHeaderNamesTheMail()
        thePreviewKeysAreEnterSXAndEsc()

        if failures > 0 {
            print("FAILED: \(failures) of \(checks) checks")
            exit(1)
        }
        print("ok: \(checks) checks passed")
    }

    // MARK: - verbs

    static func theVerbSaysWhatTheButtonDoes() {
        equal(ScheduleCopy.sendVerb(scheduled: false), "send", "no time: send")
        equal(ScheduleCopy.sendVerb(scheduled: true), "schedule", "a time: schedule")
    }

    // MARK: - drafts

    /// THE CLOSED LIST: a composer with a send time saves exactly what the same
    /// composer without one saves.
    static func aDraftNeverSavesTheSendTime() {
        var plain = composer()
        plain.replyToMessageId = 7
        var timed = plain
        timed.sendAt = Date(timeIntervalSince1970: 1_800_000_000)
        equal(DraftPayload(timed), DraftPayload(plain), "sendAt is not a draft field")
        // And the payload is not trivially equal: a words change is seen.
        var edited = timed
        edited.body = "different words"
        check(DraftPayload(edited) != DraftPayload(plain), "a body edit is a draft change")
    }

    // MARK: - the request

    static func theScheduleBodyIsTheSendBodyPlusWhen() {
        var c = composer()
        c.sendAt = Date(timeIntervalSince1970: 1_800_000_000)
        let send = json(SendBody(compose: c, draftId: nil))
        let scheduled = json(
            ScheduleBody(
                send: SendBody(compose: c, draftId: nil),
                send_at: "2027-01-15T08:00:00Z"))
        equal(scheduled["send_at"] as? String, "2027-01-15T08:00:00Z", "send_at rides along")
        equal(scheduled["confirm"] as? Bool, true, "confirm: true, as a send")
        equal(
            Set(scheduled.keys), Set(send.keys).union(["send_at"]),
            "exactly the send's keys, plus send_at")
        for (key, value) in send {
            equal(
                "\(scheduled[key] ?? "missing")", "\(value)", "\(key) is the send's own value")
        }
        // The fields a composer states, spelled the way a send spells them.
        equal(scheduled["to"] as? String, "ana@x.test", "to")
        equal(scheduled["cc"] as? String, "", "a stated empty cc is said")
        equal(scheduled["bcc"] as? String, "dana@x.test", "bcc")
        equal(scheduled["subject"] as? String, "lunch", "subject")
        equal(scheduled["body_format"] as? String, "markdown", "markdown")
        equal(scheduled["attachment_ids"] as? [Int], [11, 12], "staged files, tray order")
        check(scheduled["override_guard"] == nil, "no override_guard: the field is gone")
        check(scheduled["include_tracker"] == nil, "an untracked send says nothing")
        check(scheduled["reply_all"] == nil, "a new message never widens")
    }

    static func aScheduleNamesTheDraftItConsumes() {
        let body = json(
            ScheduleBody(
                send: SendBody(compose: composer(), draftId: 42),
                send_at: "2027-01-15T08:00:00Z"))
        equal(body["draft_id"] as? Int, 42, "draft_id is the one passed in")
        let none = json(
            ScheduleBody(
                send: SendBody(compose: composer(), draftId: nil),
                send_at: "2027-01-15T08:00:00Z"))
        check(none["draft_id"] == nil, "no draft, no key")
    }

    // MARK: - undo

    /// A DELETE /client/scheduled/{id} answer, verbatim in shape: the files
    /// in UPLOAD order (12 before 11), the request in tray order (11, 12).
    /// An older row's frozen request may still carry `override_guard`; it
    /// decodes, and the key is dropped.
    static let cancelFixture = """
        {"status":"cancelled","id":9,
         "request":{"to":"ana@x.test","cc":"bo@x.test","bcc":"dana@x.test",
           "subject":"lunch","body":"see you **there**","body_format":"markdown",
           "include_tracker":true,"attachment_ids":[11,12],"confirm":true,
           "override_guard":false},
         "attachments":[
           {"id":12,"filename":"b.pdf","mime":"application/pdf","size":20,"content_id":"b"},
           {"id":11,"filename":"a.png","mime":"image/png","size":10,"content_id":"a"}]}
        """

    static func undoReopensTheComposerFromTheCancel() {
        guard let cancelled = decode(ScheduledCancel.self, cancelFixture),
            let request = cancelled.request
        else { return }
        let when = Date(timeIntervalSince1970: 1_800_000_000)
        let state = ComposeState.reopened(
            request: request, attachments: cancelled.attachments, sendAt: when)
        equal(state.to, "ana@x.test", "to")
        equal(state.cc, "bo@x.test", "cc")
        equal(state.bcc, "dana@x.test", "bcc")
        equal(state.recipientsStated, true, "a cc on the wire is a stated audience")
        equal(state.subject, "lunch", "subject")
        equal(state.body, "see you **there**", "body")
        equal(state.includeTracker, true, "the tracker switch comes back")
        equal(state.sendAt, when, "the time is still set")
        equal(state.attachments.map(\.id), [11, 12], "tray order is the request's")
        equal(state.attachments.map(\.filename), ["a.png", "b.pdf"], "with their names")
        equal(state.stagedAttachmentIds, [11, 12], "and every file is staged")
        equal(state.draftId, nil, "the consumed draft is not named again")

        // A reply that was still deriving its audience says so again.
        let reply = ScheduledRequest(reply_to_message_id: 5, body: "yes", reply_all: true)
        let back = ComposeState.reopened(request: reply, attachments: [], sendAt: when)
        equal(back.replyToMessageId, 5, "the parent")
        equal(back.recipientsStated, false, "no cc key: the daemon derives again")
        equal(back.replyAll, true, "reply-all comes back")
        // And round-trips to the same wire it left as.
        let wire = json(SendBody(compose: back, draftId: nil))
        check(wire["cc"] == nil, "and the resend still sends no cc")
        equal(wire["reply_all"] as? Bool, true, "and still widens")
    }

    static func undoLendsDisplayStateButNeverIdentity() {
        var base = composer()
        base.forwardOfMessageId = 3
        base.forwardedSubject = "Fwd: the plan"
        base.draftId = 42
        let request = ScheduledRequest(forward_of_message_id: 3, to: "ana@x.test", body: "")
        let state = ComposeState.reopened(
            request: request, attachments: [], sendAt: nil, base: base)
        equal(state.forwardedSubject, "Fwd: the plan", "display state comes from the base")
        equal(state.forwardOfMessageId, 3, "the wire state from the request")
        check(state.id != base.id, "a fresh composer identity")
        equal(state.draftId, nil, "and never the consumed draft")
    }

    static func aCancelWhoseRequestNoLongerParsesStillDecodes() {
        let raw = #"{"status":"cancelled","id":4,"request":null,"attachments":[]}"#
        let cancelled = decode(ScheduledCancel.self, raw)
        equal(cancelled?.id, 4, "decodes")
        check(cancelled?.request == nil, "with nothing to reopen")
    }

    static func aConflictIsAlreadySent() {
        equal(
            ScheduleUndo.verdict(APIError(.unknown, 409, "already being sent")), .alreadySent,
            "409: the worker has it")
        equal(ScheduleUndo.verdict(APIError(.notFound, 404, "not found")), .failed, "404")
        equal(
            ScheduleUndo.verdict(APIError(.network, 0, "cannot reach squelch server")),
            .failed, "offline")
        equal(ScheduleUndo.alreadySentToast, "undo failed: already sent", "the words")
    }

    // MARK: - the chip

    static func theChipAndTheToastReadAsAWallClock() {
        var cal = Calendar(identifier: .gregorian)
        cal.timeZone = TimeZone(identifier: "UTC")!
        cal.locale = Locale(identifier: "en_US_POSIX")
        // Sunday 4 Oct 2026, 10:00.
        let now = date(cal, 2026, 10, 4, 10, 0)
        equal(
            ScheduleCopy.chip(date(cal, 2026, 10, 4, 18, 0), now: now, calendar: cal),
            "today 6pm", "today")
        equal(
            ScheduleCopy.chip(date(cal, 2026, 10, 5, 9, 0), now: now, calendar: cal),
            "tomorrow 9am", "tomorrow")
        equal(
            ScheduleCopy.chip(date(cal, 2026, 10, 6, 9, 30), now: now, calendar: cal),
            "Tue 9:30am", "inside the week: weekday")
        equal(
            ScheduleCopy.chip(date(cal, 2026, 10, 20, 9, 0), now: now, calendar: cal),
            "Oct 20 9am", "past the week: the date")
        equal(
            ScheduleCopy.scheduledToast(date(cal, 2026, 10, 6, 9, 0), now: now, calendar: cal),
            "scheduled for Tue 9am", "the toast")
    }

    // MARK: - the listing

    static func theListingDecodes() {
        let raw = """
            [{"id":3,"send_at":"2027-01-15T08:00:00Z","state":"failed","attempts":3,
              "last_error":"failed:gmail","maybe_sent":true,"to":"","subject":"","body":"hi",
              "reply_to_message_id":5,"forward_of_message_id":null,"attachments":[],
              "thread_id":"t1","parent_subject":"lunch","parent_from":"Ana",
              "parent_from_addr":"ana@x.test","created_at":"2027-01-14T08:00:00Z",
              "updated_at":"2027-01-15T08:07:00Z"}]
            """
        let rows = decode([ScheduledSendView].self, raw)
        equal(rows?.first?.state, "failed", "state")
        equal(rows?.first?.last_error, "failed:gmail", "reason")
        equal(rows?.first?.maybe_sent, true, "the sticky flag")
        let old = decode(
            [ScheduledSendView].self,
            raw.replacingOccurrences(of: #""maybe_sent":true,"#, with: ""))
        equal(old?.first?.maybe_sent, nil, "an older daemon without the flag still decodes")
        equal(rows?.first?.parent_subject, "lunch", "parent context")
        equal(rows?.first?.cc, nil, "a daemon without the copy lists: not said")
        let copied = decode(
            [ScheduledSendView].self,
            raw.replacingOccurrences(of: #""to":"","#, with: #""to":"","cc":"bo@x.test","bcc":"","#))
        equal(copied?.first?.cc, "bo@x.test", "cc")
        equal(copied?.first?.bcc, "", "a blank bcc is said as blank")
        equal(
            decode(ScheduledCreated.self, #"{"id":9,"send_at":"2027-01-15T08:00:00Z"}"#)?.id, 9,
            "201")
        equal(decode(ScheduledNow.self, #"{"status":"queued","id":9}"#)?.status, "queued", "202")
    }


    // MARK: - the Sent page: rows

    static func row(
        id: Int = 3, sendAt: String = "2026-10-04T13:00:00Z", state: String = "scheduled",
        attempts: Int = 0, lastError: String? = nil, maybeSent: Bool? = nil,
        created: String = "2026-10-01T08:00:00Z"
    ) -> ScheduledSendView {
        ScheduledSendView(
            id: id, send_at: sendAt, state: state, attempts: attempts, last_error: lastError,
            maybe_sent: maybeSent, to: "ana@x.test", subject: "lunch", body: "see you", reply_to_message_id: nil,
            forward_of_message_id: nil, attachments: [], thread_id: nil, parent_subject: nil,
            parent_from: nil, parent_from_addr: nil, created_at: created, updated_at: created)
    }

    static func utc() -> Calendar {
        var cal = Calendar(identifier: .gregorian)
        cal.timeZone = TimeZone(identifier: "UTC")!
        cal.locale = Locale(identifier: "en_US_POSIX")
        return cal
    }

    static func eachRowStateSaysWhatItIs() {
        let cal = utc()
        // Sunday 4 Oct 2026, 10:00 UTC; the row fires at 13:00.
        let now = date(cal, 2026, 10, 4, 10, 0)
        typealias T = ScheduledRowCopy.Trailing
        equal(
            ScheduledRowCopy.trailing(row(), now: now, calendar: cal),
            T(text: "today 1pm · in 3h", tone: .quiet), "scheduled: the chip and the countdown")
        equal(
            ScheduledRowCopy.trailing(
                row(sendAt: "2026-10-06T09:00:00Z"), now: now, calendar: cal),
            T(text: "Tue 9am · in 2d", tone: .quiet), "later in the week")
        equal(
            ScheduledRowCopy.trailing(
                row(attempts: 1, lastError: "failed:gmail"), now: now, calendar: cal),
            T(text: "retrying", tone: .busy), "a claim behind it: retrying, not the backoff's time")
        equal(
            ScheduledRowCopy.trailing(row(state: "sending", attempts: 1), now: now, calendar: cal),
            T(text: "sending", tone: .busy), "sending")
        equal(
            ScheduledRowCopy.trailing(
                row(state: "failed", attempts: 3, lastError: "failed:gmail"), now: now,
                calendar: cal),
            T(text: "didn't send", tone: .danger), "failed")
        equal(
            ScheduledRowCopy.trailing(
                row(state: "failed", attempts: 1, lastError: "failed:recovery"), now: now,
                calendar: cal),
            T(text: "may have sent", tone: .danger),
            "failed:recovery never says didn't send: it may have")
        equal(
            ScheduledRowCopy.trailing(
                row(state: "failed", attempts: 3, lastError: "failed:gmail", maybeSent: true),
                now: now, calendar: cal),
            T(text: "may have sent", tone: .danger),
            "the daemon's flag, whatever the reason code: an ambiguous last post")
        equal(
            ScheduledRowCopy.trailing(
                row(state: "failed", attempts: 3, lastError: "failed:gmail", maybeSent: false),
                now: now, calendar: cal),
            T(text: "didn't send", tone: .danger), "flag down: didn't send")
    }

    /// The phone cuts the waiting row's cell in two and leaves the rest whole.
    static func thePhoneSplitsOnlyTheWaitingRow() {
        let cal = utc()
        let now = date(cal, 2026, 10, 4, 10, 0)
        typealias P = ScheduledRowCopy.PhoneCells
        typealias T = ScheduledRowCopy.Trailing
        equal(
            ScheduledRowCopy.phoneCells(row(), now: now, calendar: cal),
            P(stamp: T(text: "in 3h", tone: .quiet), when: "today 1pm"),
            "scheduled: the countdown is the stamp, the time goes under the words")
        for other in [
            row(attempts: 1, lastError: "failed:gmail"), row(state: "sending", attempts: 1),
            row(state: "failed", attempts: 3, lastError: "failed:gmail"),
            row(state: "failed", attempts: 1, lastError: "failed:recovery"),
        ] {
            equal(
                ScheduledRowCopy.phoneCells(other, now: now, calendar: cal),
                P(stamp: ScheduledRowCopy.trailing(other, now: now, calendar: cal), when: nil),
                "\(other.state): the desktop's word, and no time under it")
        }
    }

    static func aFailedRowSaysWhy() {
        equal(
            ScheduledRowCopy.reasonLine(row(state: "failed", lastError: "failed:gmail")),
            "Gmail refused it", "failed: the human reason on line two")
        equal(
            ScheduledRowCopy.reasonLine(row(state: "failed", lastError: "failed:recovery")),
            "may have been sent, check Sent", "recovery")
        equal(
            ScheduledRowCopy.reasonLine(
                row(attempts: 1, lastError: "rejected:no_write_credential")),
            "sending is not connected, reconnect your account", "a retry says why it waits")
        equal(ScheduledRowCopy.reasonLine(row()), nil, "a plain scheduled row says nothing")
        equal(
            ScheduledRowCopy.reasonLine(row(state: "sending", lastError: "failed:gmail")), nil,
            "a sending row neither")
        equal(
            ScheduledRowCopy.reason("failed:something_new"), "something went wrong",
            "an unknown code is never shown raw")
        equal(
            ScheduledRowCopy.failedToast(row(state: "failed", lastError: "failed:gmail")),
            "didn't send: Gmail refused it", "the toast")
        equal(
            ScheduledRowCopy.failedToast(row(state: "failed", lastError: "failed:recovery")),
            "may have been sent, check Sent", "the recovery toast does not say didn't send")
        equal(
            ScheduledRowCopy.failedToast(
                row(state: "failed", lastError: "failed:crash_loop", maybeSent: true)),
            "may have been sent, check Sent", "keyed on maybe_sent, not the reason code")
        equal(
            ScheduledRowCopy.reasonLine(
                row(state: "failed", lastError: "failed:gmail", maybeSent: true)),
            "may have been sent, check Sent", "line two never says Gmail refused a maybe")
        check(
            !ScheduledRowCopy.mayHaveSent(
                row(state: "scheduled", attempts: 1, lastError: "failed:gmail", maybeSent: true)),
            "a row still owed its re-fire is not failed: no maybe copy")
        equal(
            ScheduledRowCopy.reasonLine(row(attempts: 1, lastError: "recovery:grace")),
            "waiting to confirm the last try", "the fire-time grace")
        equal(
            ScheduledRowCopy.reasonLine(row(state: "failed", lastError: "failed:search")),
            "could not check with Gmail", "a refused pre-fire search")
        for text in [
            ScheduledRowCopy.failedToast(row(state: "failed", lastError: "failed:gmail")),
            ScheduledRowCopy.alreadySending, ScheduledRowCopy.sendingNow,
            ScheduledRowCopy.noLongerScheduled,
        ] {
            check(!text.contains("\u{2014}"), "no em dash: \(text)")
        }
    }

    static func relUntilCountsDown() {
        let now = Date(timeIntervalSince1970: 1_800_000_000)
        equal(Fmt.relUntil(now.addingTimeInterval(5 * 60), now: now), "in 5m", "minutes")
        equal(Fmt.relUntil(now.addingTimeInterval(3 * 3600), now: now), "in 3h", "hours")
        equal(Fmt.relUntil(now.addingTimeInterval(2 * 86400), now: now), "in 2d", "days")
        equal(Fmt.relUntil(now.addingTimeInterval(20), now: now), "now", "under a minute")
        equal(Fmt.relUntil(now.addingTimeInterval(-600), now: now), "now", "gone by")
        equal(
            Fmt.relAge(now, now: now.addingTimeInterval(3 * 3600)), "3h",
            "the same span relAge reads the other way")
    }

    static func theOutboxSitsInFireOrder() {
        let rows = [
            row(id: 1, sendAt: "2026-10-05T09:00:00Z"),
            row(id: 7, sendAt: "2026-10-04T13:00:00Z"),
            row(id: 2, sendAt: "2026-10-04T13:00:00Z"),
            row(id: 4, sendAt: "not a date"),
        ]
        equal(
            ScheduledRowCopy.ordered(rows).map(\.id), [2, 7, 1, 4],
            "oldest send_at first, id breaks ties, an unparseable time last")
    }

    static func aFailureToastsOncePerRow() {
        var seen = ScheduledFailures()
        let failed = row(id: 3, state: "failed", lastError: "failed:gmail")
        let listing = [row(id: 1), failed, row(id: 5, state: "sending")]
        equal(seen.fresh(listing).map(\.id), [3], "the failed row toasts")
        equal(seen.fresh(listing).map(\.id), [], "and not on the next tick")
        equal(seen.fresh([failed]).map(\.id), [], "nor on a listing with only it")
        // Ids are reused: a NEW row under the same id that fails is news.
        let reused = row(
            id: 3, state: "failed", lastError: "failed:gmail", created: "2026-10-09T08:00:00Z")
        equal(seen.fresh([reused]).map(\.id), [3], "a reused id is a new failure")
        seen.clear()
        equal(seen.fresh([failed]).map(\.id), [3], "an account switch forgets")
    }

    static func aCancelsUndoRebuildsTheSchedule() {
        // The DELETE's request, as the daemon returns it: confirm STRIPPED.
        let raw = """
            {"status":"cancelled","id":9,
             "request":{"reply_to_message_id":5,"cc":"","bcc":"dana@x.test",
               "body":"yes","body_format":"markdown","include_tracker":true,
               "reply_all":true,"attachment_ids":[12,11],"group_id":4},
             "attachments":[]}
            """
        guard let cancelled = decode(ScheduledCancel.self, raw),
            let request = cancelled.request
        else { return }
        let body = ScheduledCancelUndo.body(request)
        let wire = json(ScheduleBody(send: body, send_at: "2027-01-15T08:00:00Z"))
        equal(wire["reply_to_message_id"] as? Int, 5, "the parent")
        equal(wire["cc"] as? String, "", "a stated empty cc stays stated")
        equal(wire["bcc"] as? String, "dana@x.test", "bcc")
        check(wire["to"] == nil, "no to: a reply still derives it")
        check(wire["subject"] == nil, "no subject: still derived")
        equal(wire["body"] as? String, "yes", "body")
        equal(wire["body_format"] as? String, "markdown", "format")
        equal(wire["confirm"] as? Bool, true, "confirm is the client's to set")
        check(wire["override_guard"] == nil, "no override_guard on the wire")
        check(wire["draft_id"] == nil, "no draft")
        equal(wire["include_tracker"] as? Bool, true, "tracker")
        equal(wire["reply_all"] as? Bool, true, "reply all")
        equal(wire["attachment_ids"] as? [Int], [12, 11], "the files, in the request's order")
        equal(wire["group_id"] as? Int, 4, "attribution")
        equal(wire["send_at"] as? String, "2027-01-15T08:00:00Z", "and the time beside it")
    }

    static func anUndoNeverSendsSoonerThanAsked() {
        let now = Date(timeIntervalSince1970: 1_800_000_000)
        check(
            ScheduledCancelUndo.canReschedule(at: now.addingTimeInterval(3600), now: now),
            "an hour out: re-POST at that time")
        check(
            !ScheduledCancelUndo.canReschedule(at: now.addingTimeInterval(5), now: now),
            "due inside the POST's trip: reopen instead")
        check(
            !ScheduledCancelUndo.canReschedule(at: now.addingTimeInterval(-60), now: now),
            "a failed row's past time: reopen instead")
    }

    static func aTombstoneOutlivesOnlyTheFetchesThatCouldCarryIt() {
        var t = Tombstones()
        let before = t.beginFetch()
        t.leaving(3)
        check(t.hides(3, fetchedAt: before), "on the wire: hidden from every fetch")
        check(t.isLeaving(3), "and known to be leaving")
        t.gone(3)
        let after = t.beginFetch()
        check(t.hides(3, fetchedAt: before), "a fetch that left before still drops it")
        check(!t.hides(3, fetchedAt: after), "one that left after lists the reused id")
        t.endFetch(before)
        t.endFetch(after)
        let later = t.beginFetch()
        check(!t.hides(3, fetchedAt: later), "pruned once no fetch could carry it")
        t.endFetch(later)
        // A failed DELETE shows the row again.
        let during = t.beginFetch()
        t.leaving(8)
        t.revive(8)
        check(!t.hides(8, fetchedAt: during), "revived")
        t.endFetch(during)
    }

    /// THE FLOOR IS THE OLDEST FETCH IN THE AIR. Fetches land out of order: a
    /// younger one landing first must not prune a tombstone an older one,
    /// still out, may yet carry. Fails if `prune` floors at the fetch that
    /// just landed; the counts fail if `prune` is gone altogether.
    static func aTombstoneWaitsForTheOldestFetchInTheAir() {
        var t = Tombstones()
        let old = t.beginFetch()
        t.gone(4)
        let young = t.beginFetch()
        t.endFetch(young)
        check(t.hides(4, fetchedAt: old), "the older fetch still drops the row")
        equal(t.count, 1, "kept while that fetch is out")
        t.endFetch(old)
        equal(t.count, 0, "pruned once it lands")
        t.gone(9)
        equal(t.count, 0, "nothing in the air: pruned at once")
    }

    // MARK: - review round four

    /// M1: the inline reply's editor takes the caret back when the palette
    /// closes over it, once per close, and never on a plain re-render.
    static func theEditorTakesTheCaretOncePerRequest() {
        check(EditorRefocus.returns(from: 1, to: nil, slot: 1), "palette closed over this slot")
        check(!EditorRefocus.returns(from: 2, to: nil, slot: 1), "another slot's palette")
        check(!EditorRefocus.returns(from: nil, to: 1, slot: 1), "opening is not returning")
        check(!EditorRefocus.returns(from: 1, to: 2, slot: 1), "moved, not closed")
        var seen = 0
        check(!EditorRefocus.take(0, seen: &seen), "the mount's token is not a request")
        check(EditorRefocus.take(1, seen: &seen), "a bump is")
        check(!EditorRefocus.take(1, seen: &seen), "answered once, not per render")
        check(EditorRefocus.take(2, seen: &seen), "the next close is a new request")
    }

    /// M3: an individual group sends no `to`, so the reopened composer takes
    /// its pill from the composer it came from, or from the group itself.
    static func aFanOutReopensWithItsAudience() {
        var r = ScheduledRequest()
        r.group_id = 42
        r.body = "hi all"
        let crew = SendGroup(
            id: 42, name: "Book club", slug: "book-club", mode: .individual, note: "",
            member_count: 6, members: nil, last_sent_at: nil, created_at: "", updated_at: "")
        let fromGroup = ComposeState.reopened(
            request: r, attachments: [], sendAt: nil, group: crew)
        equal(fromGroup.to, "#book-club", "the token, from the group")
        equal(fromGroup.groupMode, .individual, "the mode")
        equal(fromGroup.groupName, "Book club", "the name")
        equal(fromGroup.groupId, 42, "and the id on the wire")
        let body = json(SendBody(compose: fromGroup, draftId: nil))
        check(body["to"] == nil, "still no `to` when rescheduled")
        equal(body["group_id"] as? Int, 42, "the audience rides as group_id")

        var base = ComposeState()
        base.to = "#book-club"
        base.groupId = 42
        base.groupMode = .individual
        base.groupName = "Book club"
        let fromBase = ComposeState.reopened(request: r, attachments: [], sendAt: nil, base: base)
        equal(fromBase.to, "#book-club", "the undo takes the base's token")
        equal(fromBase.groupName, "Book club", "and its name")

        let lost = ComposeState.reopened(request: r, attachments: [], sendAt: nil)
        equal(lost.to, "", "a group nobody can describe leaves the field empty")
        equal(lost.groupId, 42, "but the id still addresses it")

        // A `to` group was expanded into real addresses: those stand.
        var expanded = r
        expanded.to = "ana@x.test, bo@x.test"
        var toGroup = crew
        toGroup.mode = .to
        let listed = ComposeState.reopened(
            request: expanded, attachments: [], sendAt: nil, group: toGroup)
        equal(listed.to, "ana@x.test, bo@x.test", "an expanded group keeps its addresses")
        equal(listed.groupMode, .to, "and learns its mode")
        // A group whose id is not the request's lends nothing.
        var other = crew
        other.id = 7
        let wrong = ComposeState.reopened(request: r, attachments: [], sendAt: nil, group: other)
        equal(wrong.to, "", "another group's token never lands")
        equal(wrong.groupName, nil, "nor its name")
    }

    /// M2: which pull-backs PUT over a keyed row holding somebody's words.
    static func aPullBackNeverSilentlyReplacesAKeyedDraft() {
        var reply = ComposeState()
        reply.replyToMessageId = 10
        var forward = ComposeState()
        forward.forwardOfMessageId = 20
        let fresh = ComposeState()
        let replyRow = draft(id: 1, replyTo: 10)
        let forwardRow = draft(id: 2, forwardOf: 20)
        let newRow = draft(id: 3)
        let all = [replyRow, forwardRow, newRow]
        check(
            PullBackDraft.wouldReplace(reply, drafts: [replyRow], open: [], keyedNewMail: false),
            "a reply over the parent's draft")
        check(
            !PullBackDraft.wouldReplace(reply, drafts: [forwardRow, newRow], open: [], keyedNewMail: false),
            "a reply with no draft to that parent")
        check(
            !PullBackDraft.wouldReplace(
                reply, drafts: [draft(id: 4, replyTo: 11)], open: [], keyedNewMail: false),
            "another parent's draft is another row")
        check(
            PullBackDraft.wouldReplace(forward, drafts: [forwardRow], open: [], keyedNewMail: false),
            "a forward over the original's draft")
        check(
            !PullBackDraft.wouldReplace(forward, drafts: [replyRow], open: [], keyedNewMail: false),
            "a forward is keyed by its original, not a parent")
        check(
            !PullBackDraft.wouldReplace(fresh, drafts: all, open: [], keyedNewMail: false),
            "new mail on drafts_v2 is a row of its own")
        check(
            PullBackDraft.wouldReplace(fresh, drafts: [newRow], open: [], keyedNewMail: true),
            "new mail on the one nil-keyed row")
        // An open composer about to flush into the same row counts too.
        var typing = ComposeState()
        typing.replyToMessageId = 10
        check(
            PullBackDraft.wouldReplace(reply, drafts: [], open: [typing], keyedNewMail: false),
            "an open reply to the same parent")
        var forwardOpen = ComposeState()
        forwardOpen.forwardOfMessageId = 10
        check(
            !PullBackDraft.wouldReplace(reply, drafts: [], open: [forwardOpen], keyedNewMail: false),
            "a forward OF the parent is not a reply to it")
        check(
            !PullBackDraft.wouldReplace(reply, drafts: [], open: [reply], keyedNewMail: false),
            "the composer itself is not a rival")
        check(
            !ScheduleCopy.replacedDraft(forward: true).contains("\u{2014}")
                && !ScheduleCopy.replacedDraft(forward: false).contains("\u{2014}"),
            "no em dashes")
    }

    /// THE M2 RULE, revised (2026-09-29): `edit` is an explicit choice, so the
    /// scheduled mail WINS. The save is armed at once, always (that is the
    /// composer opening in the DELETE's turn, `AppStore.openPulledBack`, which
    /// arms unconditionally), and a keyed draft with words in it earns a toast
    /// saying it was replaced. This is the toast's half. The list is taken
    /// AFTER the composer opened, so its own row is not a rival.
    static func anEditArmsItsSaveAndSaysWhatItReplaced() {
        func toast(
            _ s: ComposeState, _ drafts: [DraftView], open: [ComposeState] = [], own: Int? = nil,
            keyed: Bool = false
        ) -> String? {
            PullBackDraft.replacedToast(
                s, drafts: drafts, open: open, ownDraftId: own, keyedNewMail: keyed)
        }
        var reply = ComposeState()
        reply.replyToMessageId = 10
        var forward = ComposeState()
        forward.forwardOfMessageId = 20
        let fresh = ComposeState()
        equal(
            toast(reply, [draft(id: 1, replyTo: 10)]),
            "replaced your unsent draft to this thread", "a reply over the parent's draft")
        equal(
            toast(forward, [draft(id: 2, forwardOf: 20)]),
            "replaced your unsent forward of this email", "a forward: the forward's words")
        var typing = ComposeState()
        typing.replyToMessageId = 10
        equal(
            toast(reply, [], open: [typing]), "replaced your unsent draft to this thread",
            "an open composer to the same parent counts")
        equal(toast(reply, [draft(id: 4, replyTo: 11)]), nil, "no rival: nothing said")
        equal(toast(fresh, [draft(id: 3)]), nil, "new mail on drafts_v2: nothing to replace")
        equal(
            toast(fresh, [draft(id: 3)], keyed: true), "replaced your unsent draft to this thread",
            "new mail on the one nil-keyed row")
        // The pulled-back mail's own first save, landed before the list did.
        equal(
            toast(reply, [draft(id: 1, replyTo: 10)], own: 1), nil,
            "its own row is this mail, not a rival")
        equal(
            toast(reply, [draft(id: 1, replyTo: 10)], open: [typing], own: 1),
            "replaced your unsent draft to this thread",
            "but a rival that was open before it still counts")
        check(PullBackDraft.keyed(reply, keyedNewMail: false), "a reply is keyed")
        check(PullBackDraft.keyed(forward, keyedNewMail: false), "a forward is keyed")
        check(!PullBackDraft.keyed(fresh, keyedNewMail: false), "new mail on drafts_v2 is not")
        check(PullBackDraft.keyed(fresh, keyedNewMail: true), "new mail on an old daemon is")
    }

    /// Where a pulled-back mail opens, decided in the DELETE's own turn, and
    /// the pane rival closed first (MIN S1): a pane reply to the same parent
    /// writes the same keyed row, and left open its later flush would land on
    /// top of the scheduled mail.
    static func aPullBackOpensWhereItBelongs() {
        typealias R = PullBackDraft.Route
        var reply = ComposeState()
        reply.replyToMessageId = 10
        var forward = ComposeState()
        forward.forwardOfMessageId = 10
        equal(
            PullBackDraft.route(reply, threadId: "t1", pane: nil),
            R.inline(threadId: "t1", closePane: false), "a reply opens in its thread")
        equal(
            PullBackDraft.route(reply, threadId: nil, pane: nil), R.pane,
            "a reply whose parent is gone opens in the pane")
        equal(PullBackDraft.route(forward, threadId: "t1", pane: nil), R.pane, "a forward: pane")
        equal(PullBackDraft.route(ComposeState(), threadId: nil, pane: nil), R.pane, "new mail")
        var paneReply = ComposeState()
        paneReply.replyToMessageId = 10
        equal(
            PullBackDraft.route(reply, threadId: "t1", pane: paneReply),
            R.inline(threadId: "t1", closePane: true),
            "a pane reply to the same parent is closed first")
        var paneOther = ComposeState()
        paneOther.replyToMessageId = 11
        equal(
            PullBackDraft.route(reply, threadId: "t1", pane: paneOther),
            R.inline(threadId: "t1", closePane: false), "another parent's reply stays")
        var paneForward = ComposeState()
        paneForward.forwardOfMessageId = 7
        paneForward.replyToMessageId = 10
        equal(
            PullBackDraft.route(reply, threadId: "t1", pane: paneForward),
            R.inline(threadId: "t1", closePane: false), "a forward is keyed elsewhere")
        equal(
            PullBackDraft.route(reply, threadId: "t1", pane: ComposeState()),
            R.inline(threadId: "t1", closePane: false), "new mail in the pane stays")
    }

    /// The group lookup and the drafts check land AFTER the composer opened,
    /// and only on a composer that is still the pulled-back one.
    static func aLateAnswerLandsOnlyOnItsOwnComposer() {
        let mine = ComposeState()
        let other = ComposeState()
        equal(PullBackDraft.stillOpen(mine.id, pane: mine, inline: nil), .pane, "in the pane")
        equal(PullBackDraft.stillOpen(mine.id, pane: other, inline: mine), .inline, "inline")
        equal(
            PullBackDraft.stillOpen(mine.id, pane: other, inline: nil), nil,
            "sent, closed or replaced: nobody's")
        equal(PullBackDraft.stillOpen(mine.id, pane: nil, inline: nil), nil, "both empty")
        // The patch itself: the group's pill onto a composer opened without it.
        var r = ScheduledRequest()
        r.group_id = 4
        let crew = SendGroup(
            id: 4, name: "Book club", slug: "book-club", mode: .individual, note: "",
            member_count: 6, members: nil, last_sent_at: nil, created_at: "", updated_at: "")
        var open = ComposeState.reopened(request: r, attachments: [], sendAt: nil)
        equal(open.groupName, nil, "opened before the lookup came back")
        open.adoptGroup(crew, request: r)
        equal(open.groupName, "Book club", "patched with the name")
        equal(open.to, GroupToken.encode(crew), "and the fan-out token")
        var typed = ComposeState.reopened(request: r, attachments: [], sendAt: nil)
        typed.to = "someone@x.test"
        typed.adoptGroup(crew, request: r)
        equal(typed.to, "someone@x.test", "a to typed in the meantime is kept")
        var elsewhere = crew
        elsewhere.id = 5
        var untouched = ComposeState.reopened(request: r, attachments: [], sendAt: nil)
        untouched.adoptGroup(elsewhere, request: r)
        equal(untouched.groupName, nil, "another group lends nothing")
    }

    /// MIN S4: outbox ids are reused, so a fan-out's name leaves with its row,
    /// but not because of a listing that could not have seen it yet.
    static func aFanOutNameLeavesWithItsRow() {
        var names = ScheduledGroupNames()
        names.set(3, "Book club")
        let since = names.generation
        names.set(4, "Crew")
        names.prune(listed: [], since: since)
        equal(names[3], nil, "listed without it: gone")
        equal(names[4], "Crew", "named after the listing left: kept")
        names.prune(listed: [], since: names.generation)
        equal(names[4], nil, "and dropped by the next listing without it")
        names.set(5, "A")
        names.prune(listed: [5], since: names.generation)
        equal(names[5], "A", "a listed row keeps its name")
        names.set(5, nil)
        equal(names[5], nil, "a reused id scheduled with no group sheds the old name")
        names.set(6, "B")
        equal(names.take(6), "B", "a pull-back takes it")
        equal(names[6], nil, "and it is gone")
    }

    /// MIN S6: the preview has no bytes for an inline picture, and the frame
    /// drops a `cid:` image it cannot resolve, so the marker becomes a name.
    static func anInlinePictureIsNamedNotDropped() {
        let photo = OutboundAttachment(
            id: 1, filename: "photo.png", mime: "image/png", size: 10, content_id: "tok-1@passband")
        let body = "look:\n![photo.png](cid:tok-1@passband)\nnice"
        equal(
            ScheduledPreviewCopy.placeholdingInlineImages(body, attachments: [photo]),
            "look:\n[photo.png]\nnice", "the file's name in brackets")
        equal(
            ScheduledPreviewCopy.placeholdingInlineImages(
                "![old name](cid:gone@passband)", attachments: [photo]),
            "[old name]", "a token no file carries keeps its alt")
        equal(
            ScheduledPreviewCopy.placeholdingInlineImages(
                "![](cid:gone@passband)", attachments: []),
            "[image]", "and a bare one says image")
        equal(
            ScheduledPreviewCopy.placeholdingInlineImages(
                "![a](https://x.test/a.png) and (cid:x)", attachments: []),
            "![a](https://x.test/a.png) and (cid:x)", "anything else is left alone")
        check(!ScheduledPreviewCopy.renderFailed.contains("\u{2014}"), "no em dash")
    }

    // MARK: - the read-only preview

    static func thePreviewSaysWhereTheMailIs() {
        let cal = utc()
        let now = date(cal, 2026, 10, 4, 10, 0)
        typealias S = ScheduledPreviewCopy.Status
        equal(
            ScheduledPreviewCopy.status(
                row(sendAt: "2026-10-06T09:00:00Z"), now: now, calendar: cal),
            S(text: "scheduled · Tue 9am · in 2d", tone: .quiet), "scheduled: chip and countdown")
        equal(
            ScheduledPreviewCopy.status(
                row(attempts: 1, lastError: "failed:gmail"), now: now, calendar: cal),
            S(text: "retrying · Gmail refused it", tone: .busy), "retrying, with why")
        equal(
            ScheduledPreviewCopy.status(row(attempts: 2), now: now, calendar: cal),
            S(text: "retrying", tone: .busy), "retrying with no reason on file")
        equal(
            ScheduledPreviewCopy.status(row(state: "sending", attempts: 1), now: now, calendar: cal),
            S(text: "sending", tone: .busy), "sending")
        equal(
            ScheduledPreviewCopy.status(
                row(state: "failed", attempts: 1, lastError: "rejected:too_large"), now: now,
                calendar: cal),
            S(text: "didn't send · it is too large to send", tone: .danger), "failed, with why")
        equal(
            ScheduledPreviewCopy.status(
                row(state: "failed", attempts: 1, lastError: "failed:gmail", maybeSent: true),
                now: now, calendar: cal),
            S(text: "may have sent", tone: .danger), "a row that may have gone never says didn't")
        equal(
            ScheduledPreviewCopy.status(row(sendAt: "garbage"), now: now, calendar: cal),
            S(text: "scheduled", tone: .quiet), "an unreadable time says only scheduled")
        for state in ["scheduled", "sending", "failed"] {
            let text = ScheduledPreviewCopy.status(row(state: state), now: now, calendar: cal).text
            check(!text.contains("\u{2014}"), "no em dash in \(state)")
        }
    }

    static func thePreviewHeaderNamesTheMail() {
        typealias L = ScheduledPreviewCopy.Line
        // New mail: to, subject.
        equal(
            ScheduledPreviewCopy.header(row()),
            [L(label: "to", value: "ana@x.test"), L(label: "subject", value: "lunch")], "new mail")
        var untitled = row()
        untitled.subject = "  "
        untitled.to = ""
        equal(
            ScheduledPreviewCopy.header(untitled, groupName: "Book club"),
            [L(label: "to", value: "Book club"), L(label: "subject", value: "(no subject)")],
            "a fan-out with no subject")
        // A reply: to, re: parent · sender, and its own subject only if typed.
        var reply = row()
        reply.reply_to_message_id = 10
        reply.thread_id = "t1"
        reply.parent_subject = "Dinner?"
        reply.parent_from = "Bo Lee <bo@x.test>"
        reply.subject = ""
        equal(
            ScheduledPreviewCopy.header(reply),
            [L(label: "to", value: "ana@x.test"), L(label: "re", value: "Dinner? · Bo Lee")],
            "a reply")
        reply.to = ""
        equal(
            ScheduledPreviewCopy.header(reply).first,
            L(label: "to", value: ScheduledPreviewCopy.replyAudience),
            "a reply with no stated audience says the thread decides")
        // Copy lists, when the request states them.
        var copied = row()
        copied.cc = "Bo Lee <bo@x.test>"
        copied.bcc = "dana@x.test"
        equal(
            ScheduledPreviewCopy.header(copied),
            [
                L(label: "to", value: "ana@x.test"), L(label: "cc", value: "Bo Lee <bo@x.test>"),
                L(label: "bcc", value: "dana@x.test"), L(label: "subject", value: "lunch"),
            ],
            "cc and bcc lines")
        copied.cc = ""
        equal(ScheduledPreviewCopy.header(copied).map(\.label), ["to", "bcc", "subject"], "a blank cc is not printed")
        var bccOnly = row()
        bccOnly.to = ""
        bccOnly.bcc = "dana@x.test"
        equal(
            ScheduledPreviewCopy.header(bccOnly).first,
            L(label: "to", value: ScheduledPreviewCopy.nobody),
            "bcc only: no guessing at a group")
        bccOnly.bcc = ""
        equal(
            ScheduledPreviewCopy.header(bccOnly).first, L(label: "to", value: "group or bcc"),
            "nothing stated at all: the old guess")
        equal(
            ScheduledPreviewCopy.header(bccOnly, groupName: "Crew").first,
            L(label: "to", value: "Crew"), "a known group names itself")
        // A forward: to, fwd: original, subject when typed.
        var forward = row()
        forward.forward_of_message_id = 20
        forward.parent_subject = "Receipt"
        forward.parent_from = "shop@x.test"
        equal(
            ScheduledPreviewCopy.header(forward),
            [
                L(label: "to", value: "ana@x.test"), L(label: "fwd", value: "Receipt"),
                L(label: "subject", value: "lunch"),
            ],
            "a forward")
    }

    static func thePreviewKeysAreEnterSXAndEsc() {
        let keys = ScheduledPreviewCopy.keys(row())
        equal(
            keys.map(\.key), ["Escape", "Enter", "s", "x", "Backspace", "Delete"],
            "the keys, Backspace and Delete as on the row")
        equal(
            keys.map(\.verb), [.back, .edit, .sendNow, .cancel, .cancel, .cancel],
            "each to its verb")
        check(!keys.contains { $0.key == "e" || $0.key == "E" }, "never e: e is done")
        equal(
            ScheduledPreviewCopy.verbs(row()).map(\.verb.label), ["edit", "send now", "cancel"],
            "the verbs at the bottom")
        equal(
            ScheduledPreviewCopy.verbs(row()).map(\.hint), ["↵", "s", "x"], "their printed keys")
        let sending = row(state: "sending", attempts: 1)
        equal(ScheduledPreviewCopy.keys(sending).map(\.key), ["Escape"], "sending: back only")
        equal(ScheduledPreviewCopy.verbs(sending).count, 0, "sending: no verbs")
        equal(
            ScheduledPreviewCopy.keys(row(state: "failed")).count, 6,
            "a failed row can still be edited, sent or cancelled")
    }

    /// Minor 2: the ledger survives a relaunch (the store saves `keys` per
    /// account) and stays bounded.
    static func aFailureToastsOnceAcrossLaunches() {
        var first = ScheduledFailures()
        let failed = row(id: 3, state: "failed", lastError: "failed:gmail")
        equal(first.fresh([failed]).map(\.id), [3], "toasts the first time")
        var relaunched = ScheduledFailures(keys: first.keys)
        equal(relaunched.fresh([failed]).map(\.id), [], "and not after a relaunch")
        var big = ScheduledFailures()
        for i in 0..<(ScheduledFailures.cap + 5) {
            _ = big.fresh([row(id: i, state: "failed")])
        }
        equal(big.keys.count, ScheduledFailures.cap, "bounded")
        equal(big.fresh([row(id: 0, state: "failed")]).map(\.id), [0], "the oldest fell off")
        equal(
            big.fresh([row(id: ScheduledFailures.cap + 4, state: "failed")]).map(\.id), [],
            "the newest are kept")
        equal(ScheduledFailures(keys: big.keys + ["x@y"]).keys.count, ScheduledFailures.cap,
            "a loaded ledger is bounded too")
    }

    /// Minor 3: a known-empty outbox is still asked every five minutes, so a
    /// row another device scheduled turns up here.
    static func anEmptyOutboxIsStillAskedNowAndThen() {
        let t0 = Date(timeIntervalSince1970: 1_800_000_000)
        check(ScheduledPoll.due(knownEmpty: false, loadedAt: t0, now: t0), "not empty: every tick")
        check(ScheduledPoll.due(knownEmpty: true, loadedAt: nil, now: t0), "never loaded: ask")
        check(
            !ScheduledPoll.due(knownEmpty: true, loadedAt: t0, now: t0.addingTimeInterval(60)),
            "empty a minute ago: skip")
        check(
            ScheduledPoll.due(
                knownEmpty: true, loadedAt: t0,
                now: t0.addingTimeInterval(ScheduledPoll.emptyEvery)),
            "empty five minutes ago: ask again")
    }

    /// Minor 8: the Sent cursor through a listing that changed under it.
    static func theCursorFollowsItsRow() {
        equal(ScheduledCursor.reanchor(1, old: [5, 6, 7], new: [4, 5, 6, 7]), 2, "a row inserted above")
        equal(ScheduledCursor.reanchor(2, old: [5, 6, 7], new: [6, 7]), 1, "a row dropped above")
        equal(ScheduledCursor.reanchor(1, old: [5, 6, 7], new: [5, 7]), 1, "its own row gone: the next")
        equal(ScheduledCursor.reanchor(2, old: [5, 6, 7], new: [5, 6]), 1, "the last one gone: clamped")
        equal(ScheduledCursor.reanchor(0, old: [5], new: []), 0, "the group emptied")
        equal(ScheduledCursor.reanchor(4, old: [5, 6], new: [5, 6, 7]), 5, "a sent row keeps its mail")
        equal(ScheduledCursor.reanchor(4, old: [5, 6], new: [6]), 3, "and the other way")
        equal(ScheduledCursor.reanchor(0, old: [5, 6], new: [5, 6]), 0, "nothing moved")
        // MIN S3: an outbox row above a sent row left in the same update that
        // shortened the page. Re-anchor then clamp keeps the sent mail; clamp
        // then re-anchor landed one row too high, on the outbox row.
        equal(
            ScheduledCursor.follow(2, old: [5, 6], new: [6], count: 2), 1,
            "the sent row under the cursor stays under it")
        let clampFirst = ScheduledCursor.reanchor(min(2, 1), old: [5, 6], new: [6])
        check(clampFirst != 1, "the other order is the bug (lands on \(clampFirst))")
        equal(ScheduledCursor.follow(3, old: [], new: [], count: 2), 1, "off Sent: the plain clamp")
        equal(ScheduledCursor.follow(0, old: [], new: [], count: 0), 0, "an empty page")
    }

    /// Minors 5 and 9: a row an earlier claim may have put in Gmail is never
    /// re-POSTed by an undo, and a reopened composer says so, or says the
    /// time has passed, before anyone presses a key.
    static func aRowGmailMayHaveIsNeverResentBlind() {
        let now = Date(timeIntervalSince1970: 1_800_000_000)
        let future = "2027-06-01T08:00:00Z"
        let past = "2026-01-01T08:00:00Z"
        check(!ScheduledCancelUndo.mayHaveReachedGmail(row()), "a fresh row")
        check(ScheduledCancelUndo.mayHaveReachedGmail(row(attempts: 1)), "a claimed row")
        check(
            ScheduledCancelUndo.mayHaveReachedGmail(row(state: "failed", maybeSent: true)),
            "a maybe-sent row")
        equal(ScheduledCancelUndo.reopenError(row(sendAt: future), now: now), nil, "nothing to say")
        equal(
            ScheduledCancelUndo.reopenError(row(sendAt: past, state: "failed"), now: now),
            ScheduleCopy.timePassed, "a failed row's time has gone by")
        equal(
            ScheduledCancelUndo.reopenError(
                row(sendAt: past, state: "failed", maybeSent: true), now: now),
            ScheduleCopy.mayHaveReachedGmail, "the Gmail doubt outranks the clock")
        equal(
            ScheduledCancelUndo.reopenError(row(sendAt: future, attempts: 2), now: now),
            ScheduleCopy.mayHaveReachedGmail, "a retrying row too")
        check(!ScheduleCopy.mayHaveReachedGmail.contains("\u{2014}"), "no em dash")
    }

    /// M3, the row half: new mail with an empty `to` is a fan-out or bcc only.
    static func aFanOutRowSaysWhoItIsFor() {
        equal(ScheduledRowCopy.emptyRecipient(groupName: "Book club"), "Book club", "known group")
        equal(ScheduledRowCopy.emptyRecipient(groupName: nil), "group or bcc", "unknown")
        equal(ScheduledRowCopy.emptyRecipient(groupName: "  "), "group or bcc", "blank name")
    }

    static func draft(id: Int, replyTo: Int? = nil, forwardOf: Int? = nil) -> DraftView {
        DraftView(
            id: id, reply_to_message_id: replyTo, forward_of_message_id: forwardOf,
            thread_id: nil, parent_subject: nil, parent_from: nil, parent_from_addr: nil,
            to: "", cc: nil, bcc: nil, subject: "", body: "words",
            created_at: "", updated_at: "", attachments: nil)
    }

    // MARK: - fixtures

    /// A new message with every field a send states: a stated (empty) cc, a
    /// bcc, a subject, two staged files.
    static func composer() -> ComposeState {
        var c = ComposeState()
        c.to = "ana@x.test"
        c.bcc = "dana@x.test"
        c.recipientsStated = true
        c.subject = "lunch"
        c.body = "see you there"
        c.attachments = [
            ComposeAttachment(id: 11, filename: "a.png", mime: "image/png", size: 10, contentId: "a"),
            ComposeAttachment(
                id: 12, filename: "b.pdf", mime: "application/pdf", size: 20, contentId: "b"),
        ]
        return c
    }

    static func json(_ value: some Encodable) -> [String: Any] {
        guard let data = try? JSONEncoder().encode(value),
            let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
        else {
            check(false, "encodes to a JSON object")
            return [:]
        }
        return obj
    }

    static func decode<T: Decodable>(_ type: T.Type, _ raw: String) -> T? {
        do {
            return try JSONDecoder().decode(T.self, from: Data(raw.utf8))
        } catch {
            check(false, "decodes \(T.self): \(error)")
            return nil
        }
    }

    static func date(_ cal: Calendar, _ y: Int, _ m: Int, _ d: Int, _ h: Int, _ min: Int) -> Date {
        cal.date(from: DateComponents(year: y, month: m, day: d, hour: h, minute: min))!
    }

    // MARK: - assert

    static func check(_ ok: Bool, _ what: String) {
        checks += 1
        if !ok {
            failures += 1
            print("  FAIL \(what)")
        }
    }

    static func equal<T: Equatable>(_ got: T?, _ want: T?, _ what: String) {
        checks += 1
        if got != want {
            failures += 1
            print("  FAIL \(what): got \(String(describing: got)), want \(String(describing: want))")
        }
    }
}

// The app's `String.trimmed` lives in Views/ConnectView.swift, which this pure
// suite does not build; GroupToken reads it.
extension String {
    var trimmed: String { trimmingCharacters(in: .whitespacesAndNewlines) }
}
