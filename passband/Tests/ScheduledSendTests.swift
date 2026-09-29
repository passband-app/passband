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
        equal(ScheduleCopy.overrideVerb(scheduled: false), "send anyway", "override, no time")
        equal(
            ScheduleCopy.overrideVerb(scheduled: true), "schedule anyway", "override, a time")
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
        let send = json(SendBody(compose: c, override: false, draftId: nil))
        let scheduled = json(
            ScheduleBody(
                send: SendBody(compose: c, override: false, draftId: nil),
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
        equal(scheduled["override_guard"] as? Bool, false, "no override on a first try")
        check(scheduled["include_tracker"] == nil, "an untracked send says nothing")
        check(scheduled["reply_all"] == nil, "a new message never widens")

        let override = json(
            ScheduleBody(
                send: SendBody(compose: c, override: true, draftId: nil), send_at: "x"))
        equal(override["override_guard"] as? Bool, true, "schedule anyway carries consent")
    }

    static func aScheduleNamesTheDraftItConsumes() {
        let body = json(
            ScheduleBody(
                send: SendBody(compose: composer(), override: false, draftId: 42),
                send_at: "2027-01-15T08:00:00Z"))
        equal(body["draft_id"] as? Int, 42, "draft_id is the one passed in")
        let none = json(
            ScheduleBody(
                send: SendBody(compose: composer(), override: false, draftId: nil),
                send_at: "2027-01-15T08:00:00Z"))
        check(none["draft_id"] == nil, "no draft, no key")
    }

    // MARK: - undo

    /// A DELETE /client/scheduled/{id} answer, verbatim in shape: the files
    /// in UPLOAD order (12 before 11), the request in tray order (11, 12).
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
        equal(state.guardKinds, [], "no verdict is carried over")

        // A reply that was still deriving its audience says so again.
        let reply = ScheduledRequest(reply_to_message_id: 5, body: "yes", reply_all: true)
        let back = ComposeState.reopened(request: reply, attachments: [], sendAt: when)
        equal(back.replyToMessageId, 5, "the parent")
        equal(back.recipientsStated, false, "no cc key: the daemon derives again")
        equal(back.replyAll, true, "reply-all comes back")
        // And round-trips to the same wire it left as.
        let wire = json(SendBody(compose: back, override: false, draftId: nil))
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
              "last_error":"failed:gmail","to":"","subject":"","body":"hi",
              "reply_to_message_id":5,"forward_of_message_id":null,"attachments":[],
              "thread_id":"t1","parent_subject":"lunch","parent_from":"Ana",
              "parent_from_addr":"ana@x.test","created_at":"2027-01-14T08:00:00Z",
              "updated_at":"2027-01-15T08:07:00Z"}]
            """
        let rows = decode([ScheduledSendView].self, raw)
        equal(rows?.first?.state, "failed", "state")
        equal(rows?.first?.last_error, "failed:gmail", "reason")
        equal(rows?.first?.parent_subject, "lunch", "parent context")
        equal(
            decode(ScheduledCreated.self, #"{"id":9,"send_at":"2027-01-15T08:00:00Z"}"#)?.id, 9,
            "201")
        equal(decode(ScheduledNow.self, #"{"status":"queued","id":9}"#)?.status, "queued", "202")
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
