// What a notification for one event SAYS, and the userInfo keys a tap on it
// routes by. Split out of Notifier.swift because there are now two posters of
// the same banner and they must agree exactly: the app, from the live event
// feed, and the notification service extension, rewriting a push that arrived
// while the app was not running. A banner whose copy or whose routing keys
// depended on which of the two posted it would be a visible seam.
//
// Pure by construction: same event in, same banner out. Nothing here posts,
// reads the keychain, or knows an account exists — `Notifier` and the extension
// each fold the account in on their own side.
//
// It also answers the question that comes BEFORE the copy, and for the same
// reason: `routing(for:)`. A sealed event is not an event banner at all (see
// docs/NOTIFY.md §11.6), and the two posters plus the live feed have to agree
// about that or one of them puts a dead tap on a lock screen.

import Foundation

enum EventBanner {
    // MARK: - userInfo keys

    /// A tap routes on the thread id, WITHIN the account named by the account
    /// id. Both are read on whatever queue the system delivers the tap to, and
    /// both must survive being written to disk by the system and read back into
    /// a later launch — hence strings, and hence the account id as its uuid
    /// string rather than as a UUID.
    static let threadKey = "passband.thread_id"
    static let eventKey = "passband.event_id"
    static let messageKey = "passband.message_id"
    static let authKey = "passband.is_auth"
    /// The sender's ADDRESS, on auth banners only, so a tap can put up the
    /// reveal with a name on it before the sealed list has loaded. Never a
    /// subject or summary: those can hold the code itself.
    static let senderKey = "passband.sender"
    /// The posting account's uuid, as a string (userInfo has to survive being
    /// written to disk by the system and read back into a later launch).
    static let accountKey = "passband.account_id"
    /// Where the tap goes when there is no thread to open. Carried ONLY by the
    /// background auth banners; its absence is how every event banner says
    /// "open the thread named in `threadKey`".
    static let routeKey = "passband.route"
    static let authRoute = "auth"
    /// The Settings test banner. It routes nowhere, but it is marked because
    /// the system has to be told to DRAW it — see `presentation` — and because
    /// a tap on it is not a human opening their mail and must not be counted
    /// as one.
    static let testRoute = "test"
    /// A new Passband version is ready. The tap fronts the window, where the
    /// update card is already waiting, and is not a mail open either.
    static let updateRoute = "update"
    /// An agent left a draft and asked the owner to review it. The banner
    /// names a draft by its row id AND its creation second, which together
    /// bind the tap to one draft: ids are reused once a draft is sent or
    /// discarded (see `AgentDraftLink`). No thread, no event.
    static let agentDraftRoute = "agent_draft"
    static let draftKey = "passband.draft_id"
    static let draftCreatedKey = "passband.draft_created"

    /// Where a notification tap goes, decided from its userInfo alone. Pure,
    /// so the one rule that matters most here is pinned by a test: a tap on
    /// AUTH mail never fetches a body. It opens the audited reveal (it has the
    /// message id the reveal needs) or, without one, the Auth page.
    enum TapRoute: Equatable, Sendable {
        case update
        case test
        /// An agent's draft to review. `accountId` nil only when an enriched
        /// banner lost its account key, which the tap queue reads as "the
        /// active account".
        case agentDraft(accountId: UUID?, draftId: Int, created: Int)
        case reveal(messageId: Int, threadId: String, sender: String)
        case authPage
        case message(Int)
        case event(UnresolvedPush)
        case thread(String?)
    }

    static func tapRoute(_ userInfo: [AnyHashable: Any]) -> TapRoute {
        let route = userInfo[routeKey] as? String
        if route == updateRoute { return .update }
        if route == testRoute { return .test }
        // BEFORE anything else reads the payload: a draft push carries no
        // message, thread or event, and must never fall through to them.
        if route == agentDraftRoute {
            let id = (userInfo[draftKey] as? Int) ?? (userInfo[draftKey] as? String).flatMap(Int.init)
            let created =
                (userInfo[draftCreatedKey] as? Int) ?? (userInfo[draftCreatedKey] as? String).flatMap(Int.init)
            if let id, id > 0, let created, created > 0 {
                return .agentDraft(
                    accountId: (userInfo[accountKey] as? String).flatMap(UUID.init(uuidString:)),
                    draftId: id, created: created)
            }
        }
        if let push = draftPush(userInfo["event_id"] as? String) {
            return .agentDraft(accountId: push.accountId, draftId: push.draftId, created: push.created)
        }
        let messageId =
            (userInfo[messageKey] as? Int) ?? (userInfo[messageKey] as? String).flatMap(Int.init)
        let flagged = (userInfo[authKey] as? Bool) ?? false
        if flagged || route == authRoute {
            guard let messageId else { return .authPage }
            return .reveal(
                messageId: messageId, threadId: userInfo[threadKey] as? String ?? "",
                sender: userInfo[senderKey] as? String ?? "")
        }
        if let messageId { return .message(messageId) }
        if let unresolved = unresolvedPush(userInfo["event_id"] as? String) {
            return .event(unresolved)
        }
        return .thread(userInfo[threadKey] as? String)
    }

    /// The relay's original push survives when device enrichment times out.
    /// Keep its account-qualified event handle so a tap can resolve the email.
    struct UnresolvedPush: Equatable, Sendable {
        var accountId: UUID
        var eventId: Int
    }

    static func unresolvedPush(_ raw: String?) -> UnresolvedPush? {
        guard let raw, let colon = raw.lastIndex(of: ":"),
            let account = UUID(uuidString: String(raw[..<colon])),
            let event = Int(raw[raw.index(after: colon)...]), event > 0 else { return nil }
        return UnresolvedPush(accountId: account, eventId: event)
    }

    /// The pusher's draft ping, `"<account>:draft:<id>:<created>"`: the tag
    /// the client registered, a literal, the draft's row id, and its creation
    /// time in Unix seconds. STRICT like `AgentDraftLink`: exactly that shape,
    /// numbers written the one plain way, nothing looser.
    ///
    /// Distinct from an event push by construction: `unresolvedPush` splits at
    /// the LAST colon, so this shape's account half is never a UUID there.
    struct DraftPush: Equatable, Sendable {
        var accountId: UUID
        var draftId: Int
        var created: Int
    }

    static func draftPush(_ raw: String?) -> DraftPush? {
        guard let raw, raw.count <= 256 else { return nil }
        let parts = raw.split(separator: ":", omittingEmptySubsequences: false)
        guard parts.count == 4, parts[1] == "draft",
            let account = UUID(uuidString: String(parts[0])),
            let id = plainNumber(parts[2], maxDigits: 12),
            let created = plainNumber(parts[3], maxDigits: 11)
        else { return nil }
        return DraftPush(accountId: account, draftId: id, created: created)
    }

    /// A positive integer written the one plain way: ASCII digits, no sign,
    /// no leading zero.
    private static func plainNumber(_ text: Substring, maxDigits: Int) -> Int? {
        guard !text.isEmpty, text.count <= maxDigits, text.first != "0",
            text.allSatisfy({ $0.isASCII && $0.isNumber }), let n = Int(text), n > 0
        else { return nil }
        return n
    }

    /// A draft's `created_at` (RFC 3339, with or without fractional seconds)
    /// as the whole Unix second a push or link names it by. nil when the
    /// stamp cannot be read, which never matches anything.
    static func draftSecond(_ createdAt: String) -> Int? {
        guard let date = eventDate(createdAt) else { return nil }
        return Int(date.timeIntervalSince1970.rounded(.down))
    }

    /// What the banner for an agent's draft says. Who it is to and what it is
    /// about, both the agent's own words in the owner's mailbox; never the
    /// body. Short, because a lock screen truncates.
    static func agentDraftCopy(to: String, subject: String, isReply: Bool, parentFrom: String?)
        -> (title: String, body: String)
    {
        // Quote-aware: `"Okafor, Maya" <maya@x>` is one recipient.
        let firstTo = SenderID.recipients(to).first ?? ""
        let named = firstTo.isEmpty ? (parentFrom ?? "") : firstTo
        let who =
            named.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            ? "" : SenderID.resolve(named).displayName
        let topic = subject.trimmingCharacters(in: .whitespacesAndNewlines)
        let lead = who.isEmpty ? "" : (isReply ? "Reply to \(who)" : "To \(who)")
        let body: String
        switch (lead.isEmpty, topic.isEmpty) {
        case (false, false): body = "\(lead): \(topic)"
        case (false, true): body = lead
        case (true, false): body = topic
        case (true, true): body = "Open it to read it and send."
        }
        return ("Your agent left a draft", body)
    }

    // MARK: - routing

    /// What one frame off the event feed IS. The whole decision, in one pure
    /// function, because it is the fork the notify lane added and the two
    /// answers could hardly be further apart: one opens a thread, the other
    /// must never touch one.
    enum Routing: Equatable, Sendable {
        /// The ordinary event: a banner naming the sender, whose tap opens the
        /// thread the mail is in.
        case threadBanner
        /// Auth mail. The event is a TRIGGER and not a notification: the row
        /// carries no subject and its `created_at` is when triage emitted it,
        /// not when the mail arrived, so nothing about the banner may be built
        /// from it beyond the kind and the sender. `/client/sealed` is the
        /// source of truth for this legacy lookup route.
        case authSignal(SealedKind)
    }

    /// Route one event. `sealed_kind` present is the entire test — see
    /// docs/NOTIFY.md §11.6. Deliberately NOT a check on tier, kind or
    /// importance: the daemon stamps a sealed row `urgent`/`signal` like any
    /// other urgent mail, so those fields cannot tell the two apart, and a
    /// sealed event routed as a thread banner would put a dead tap on screen
    /// (`thread_guard_and_subject` 404s a sealed thread) beside the poller's
    /// own banner for the same code.
    static func routing(for event: Event) -> Routing {
        guard let kind = event.sealed_kind else { return .threadBanner }
        return .authSignal(kind)
    }

    /// WHICH EVENT A NOTIFICATION IS ABOUT, as "<account>:<event id>", from
    /// either shape the phone sees: a banner the app posted, or a push the
    /// service extension enriched (both carry `accountKey` + `eventKey`), or
    /// a push it could not enrich (the relay's raw `event_id`). nil for
    /// anything about no single event (auth, tests, updates). Two
    /// notifications with one identity are the same mail told twice.
    static func eventIdentity(_ userInfo: [AnyHashable: Any]) -> String? {
        if let account = userInfo[accountKey] as? String,
            let event = (userInfo[eventKey] as? Int) ?? (userInfo[eventKey] as? NSNumber)?.intValue
        {
            return "\(account.lowercased()):\(event)"
        }
        if let push = unresolvedPush(userInfo["event_id"] as? String) {
            return "\(push.accountId.uuidString.lowercased()):\(push.eventId)"
        }
        return nil
    }

    /// The same identity for an event off the feed, before any notification.
    static func eventIdentity(account: UUID, event: Int) -> String {
        "\(account.uuidString.lowercased()):\(event)"
    }

    /// BACKLOG: an event that happened before the feed connected, replayed
    /// from the cursor the app last saved. On the phone a push already put it
    /// on the lock screen while the app was closed, so bannering it again the
    /// moment the app opens is an old notification popping up on open. Live
    /// events (after the connection) are never backlog. `grace` absorbs clock
    /// skew between the daemon and the device. An unreadable stamp is never
    /// backlog: a missed banner is worse than a repeated one.
    static func isBacklog(createdAt: String, connectedAt: Date, grace: TimeInterval = 15) -> Bool {
        guard let created = eventDate(createdAt) else { return false }
        return created < connectedAt.addingTimeInterval(-grace)
    }

    /// RFC 3339, with or without fractional seconds (the daemon writes both
    /// shapes across versions).
    static func eventDate(_ text: String) -> Date? {
        plainStamp.date(from: text) ?? fractionalStamp.date(from: text)
    }

    /// Made once: a reconnect's replay parses a stamp per frame on the main
    /// actor. ISO8601DateFormatter is safe to share for parsing.
    nonisolated(unsafe) private static let plainStamp = ISO8601DateFormatter()
    nonisolated(unsafe) private static let fractionalStamp: ISO8601DateFormatter = {
        let f = ISO8601DateFormatter()
        f.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return f
    }()

    static func shouldPresentForeground(appActive: Bool, windowVisible: Bool, isTest: Bool, isAuth: Bool) -> Bool {
        isTest || isAuth || !(appActive && windowVisible)
    }

    // MARK: - login code

    /// A login code the daemon formatted into an auth event's `one_line`.
    struct LoginCode: Equatable, Sendable {
        var service: String
        /// Exactly as the sender printed it: zeros, case, spaces and hyphens.
        var code: String

        /// What goes on the pasteboard. "123 456" is how a sender DISPLAYS six
        /// digits and a verification field takes them without the gap, so an
        /// all-digit code loses its separators. Anything with a letter in it
        /// is copied as printed: "ABC-DEF" may well want its hyphen.
        var pasteable: String {
            let bare = code.filter { $0 != " " && $0 != "-" }
            return bare.allSatisfy(\.isNumber) ? bare : code
        }
    }

    /// How long after the event a code is still worth putting in front of
    /// somebody. The daemon only formats a code for mail under ten minutes
    /// old, but the feed REPLAYS from its cursor after a reconnect, and a lid
    /// opened in the morning must not be greeted by last night's code.
    static let loginCodeWindow: TimeInterval = 10 * 60

    private static let loginCodeMarker = " login code is "

    /// The code an auth event carries, or nil. The daemon writes exactly
    /// "Your <service> login code is <code>" (`LoginCode::notification` in
    /// squelch-core) and only after checking the code against the mail, so
    /// this is a parse of our own format and not a guess at somebody's prose.
    /// The shape checks mirror the daemon's: anything else is a descriptive
    /// sentence that merely reads like one, and that stays a banner.
    static func loginCode(for event: Event, now: Date = Date()) -> LoginCode? {
        guard event.isAuth, event.sealed_kind == nil, event.one_line.hasPrefix("Your "),
            let marker = event.one_line.range(of: loginCodeMarker),
            let created = Fmt.date(event.created_at),
            abs(now.timeIntervalSince(created)) <= loginCodeWindow
        else { return nil }
        let service = event.one_line[event.one_line.index(event.one_line.startIndex, offsetBy: 5)..<marker.lowerBound]
            .trimmingCharacters(in: .whitespaces)
        let code = String(event.one_line[marker.upperBound...])
        let symbols = code.filter { $0.isASCII && ($0.isLetter || $0.isNumber) }.count
        guard !service.isEmpty, service.count <= 32, (4...12).contains(symbols), code.count <= 24,
            code == code.trimmingCharacters(in: .whitespaces),
            code.allSatisfy({ $0.isASCII && ($0.isLetter || $0.isNumber || $0 == " " || $0 == "-") })
        else { return nil }
        return LoginCode(service: service, code: code)
    }

    /// Whether a parsed code floats or stays a banner. `focused` is nil when
    /// the system will not say (permission never granted, or taken away), and
    /// that reads as NOT focused: somebody who asked for a code and got
    /// nothing is worse off than somebody who got a panel during a Focus.
    static func floatsLoginCode(enabled: Bool, respectFocus: Bool, focused: Bool?) -> Bool {
        enabled && !(respectFocus && focused == true)
    }

    // MARK: - content mapping

    /// The display copy for one event. Pure: same event in, same banner out.
    struct Copy: Equatable {
        var title: String
        var body: String
        var threadIdentifier: String
        var sound: Bool
    }

    static func copy(for event: Event) -> Copy {
        // "Sarah Chen <sarah@acme.com>" is not a notification title, and
        // neither is "bounce-1234@em.brand.com": `readableName` never answers
        // with an address, which is the one promise a bold line on a lock
        // screen has to keep. The exact address is one tap away in the thread.
        let sender = flatten(SenderID.readableName(event.sender), max: 64)
        let summary = flatten(event.one_line, max: 240)
        // Coalescing key: events on one thread stack into ONE group. The
        // fallback must be unique, or an empty thread id would glue unrelated
        // mail together.
        let group =
            event.thread_id.isEmpty ? "passband.event.\(event.id)" : event.thread_id

        // A read receipt on the user's OWN tracked mail, and NOT a triage
        // verdict: `sender` is the account's own address (using it as the title
        // would read as mail from yourself), `one_line` is the sent subject, and
        // `importance` is a placeholder. It renders on its own terms.
        if event.kind == .opened {
            return Copy(
                title: "Opened",
                body: summary.isEmpty ? "Someone opened your email." : "Opened: \(summary)",
                threadIdentifier: group,
                // News, not an obligation — no chime.
                sound: false)
        }

        // NO second line, for any kind. Banners used to say "needs attention"
        // on urgent mail and "has a deadline" on dated mail, and a banner that
        // opens by telling the human they are behind is one they flinch away
        // from rather than tap. The date was never there either: the event's
        // deadline is a snapshot taken at triage, and a banner sits on a lock
        // screen for hours. The sender, the summary, and the chime carry all
        // the weight; the thread has the live chip.
        return Copy(
            title: sender.isEmpty ? "Passband" : sender,
            // An empty one_line means triage stored no summary; say something
            // true rather than posting a blank banner.
            body: summary.isEmpty ? "New mail worth your attention." : summary,
            threadIdentifier: group,
            // Sound only for the time-bound kinds — a chime per surfaced email
            // is how a notification stream gets muted wholesale.
            sound: event.isAuth || event.kind != .surfaced)
    }

    /// The AUTH banner's copy: a mailbox has just been sent a login code (or a
    /// reset, or a sign-in alert), and the only thing this notification exists
    /// to say is which mailbox to go to and who wants the code.
    ///
    /// WHAT IS DELIBERATELY NOT IN IT: the code, and the subject line that so
    /// often IS the code ("725104 is your Acme code" is a real subject). The
    /// only two inputs are the kind and the from address — both metadata, both
    /// the same class `/client/sealed` already serves — so there is no way for
    /// a body to reach a lock screen through here even by accident. A reveal is
    /// audited server-side and belongs to the account the human is looking at.
    ///
    /// THREE POSTERS, ONE COPY: the background watcher's poll (`postAuth`), the
    /// live feed's sealed event, and the notification service extension
    /// rewriting a push that arrived while the app was not running. Nothing in
    /// the wording may depend on which of them got there first.
    ///
    /// `accountName` is nil where the poster does not have one — the extension
    /// reads credentials out of the shared keychain and has no access to the
    /// app's account labels — and then the label stands alone rather than
    /// trailing an empty separator.
    static func authCopy(kind: SealedKind?, sender: String, accountName: String?) -> Copy {
        // The kind leads, the mailbox follows: WHICH account this is happening
        // in is the entire reason the banner is worth reading, and `AuthCopy`
        // is the app's one vocabulary for the other half ("sealed" is internal
        // jargon and never reaches a human).
        let label = AuthCopy.label(kind)
        let who = flatten(SenderID.readableName(sender), max: 64)
        return Copy(
            title: accountName.map { "\(label) · \($0)" } ?? label,
            body: who.isEmpty ? "New auth mail." : "from \(who)",
            // One group per account's auth mail: a login code and the sign-in
            // alert behind it are the same conversation. Account-prefixed by
            // the poster, exactly as an event's group is, because two daemons'
            // auth mail is not one conversation.
            threadIdentifier: authGroup,
            // Always a chime. A login code is the definition of time-bound — it
            // expires while you are not looking at it.
            sound: true)
    }

    /// The coalescing group every auth banner shares, before the poster
    /// prefixes it with the account.
    static let authGroup = "passband.auth"

    /// Collapse to one line and cap the length: notification text is a small
    /// fixed box.
    static func flatten(_ s: String, max: Int) -> String {
        let flat = s.split(whereSeparator: { $0.isNewline || $0 == "\t" })
            .joined(separator: " ")
            .trimmingCharacters(in: .whitespacesAndNewlines)
        return Fmt.truncate(flat, max)
    }
}

/// Keeps the most recent explicit tap until credentials and the account are ready.
/// A tap arriving during bootstrap must not be rejected against an unloaded index.
struct NotificationTapQueue<Target> {
    struct Tap {
        var target: Target
        var accountId: UUID?
    }
    private(set) var pending: Tap?
    private var parked = false

    mutating func enqueue(_ target: Target, accountId: UUID?) {
        pending = Tap(target: target, accountId: accountId)
        parked = false
    }

    /// Failed switches wait for a successful connection or another explicit tap.
    /// A newer tap that arrived during the switch is already a fresh request.
    mutating func park(_ tap: Tap) {
        guard pending == nil else { return }
        pending = tap
        parked = true
    }

    mutating func connectionBecameReady() { parked = false }

    mutating func take(connected: Bool) -> Tap? {
        guard connected, !parked else { return nil }
        defer { pending = nil }
        return pending
    }
}
