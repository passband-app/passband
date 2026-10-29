// The notification service extension: what turns a blind push into a banner
// that says something.
//
// The relay is blind BY DESIGN (see squelch-relay/src/handlers.rs) — it carries
// an event id, a collapse id, and a generic "New mail surfaced" alert, because
// neither it nor Apple is entitled to know what the mail says. `mutable-content`
// on that push hands the payload here first, inside the user's own device,
// where the daemon can be asked directly. This process is the only place the
// two halves meet.
//
// EVERYTHING HERE FAILS OPEN, to the alert the relay already wrote. No daemon,
// no credentials, no network, no time left: the human still gets told there is
// mail, just not what it is. A silent notification would be worse than a vague
// one.

import Foundation
import UserNotifications

/// Which mailbox, and which of its events. The push's `event_id` is
/// `"<account uuid>:<event id>"` — the tag the client registered, joined to a
/// per-daemon int that means nothing without it. An untagged id (a bare number,
/// from a daemon or a client that predates the tag) is unroutable ON PURPOSE:
/// two accounts' event 41 are different mail, and guessing between them would
/// show the wrong mailbox's business on the lock screen.
struct PushRoute {
    let accountId: UUID
    let eventId: Int

    /// The relay's payload key, verbatim.
    static let eventIdKey = "event_id"

    init?(userInfo: [AnyHashable: Any]) {
        guard let route = EventBanner.unresolvedPush(userInfo[Self.eventIdKey] as? String) else { return nil }
        self.accountId = route.accountId
        self.eventId = route.eventId
    }
}

/// NOT main-actor isolated. The fetch and the expiry timer race for the same
/// one-shot handler, and an earlier version serialized them by isolating the
/// whole class to the main actor — on the ASSUMPTION that the system calls both
/// callbacks on the main queue. Nothing documents that, and under Swift 6 an
/// isolated override called from anywhere else is a runtime trap: the extension
/// dies, and the system delivers the relay's generic alert in its place. A
/// lock makes no assumption about threads at all, so the shared state lives
/// behind one, which is also what `@unchecked Sendable` is promising.
final class NotificationService: UNNotificationServiceExtension, @unchecked Sendable {
    /// STATIC, so the two fetches below can be static too. See the launch
    /// site: they run as concurrent children and need nothing of `self`.
    private nonisolated(unsafe) static let session = Sessions.ephemeral(
        timeout: 10, resource: 20,
        cachePolicy: .reloadIgnoringLocalCacheData, emptyHeaders: true)

    /// A SECOND SESSION, ON A SHORTER LEASH, for the badge count alone.
    ///
    /// The two fetches are awaited together, so the banner cannot be delivered
    /// until the slower one finishes — a stalled count would delay the alert
    /// itself. That is the wrong way round: the badge is a nicety and the banner
    /// is the reason this process exists, so the count gets a fraction of the
    /// time and drops out quietly when it cannot be had cheaply.
    ///
    /// ITS OWN SESSION rather than a shorter `timeoutInterval` on the request,
    /// because the configuration's `timeoutIntervalForRequest` and the request's
    /// own value both apply and which one wins is not a thing to guess at in a
    /// process that gets killed for being slow. Two configurations, two
    /// unambiguous ceilings.
    ///
    /// Not a theoretical hazard: reads of this band go through the daemon's
    /// store mutex, and a slow one there is a thing that has actually happened.
    private nonisolated(unsafe) static let countSession = Sessions.ephemeral(
        timeout: 4, resource: 8,
        cachePolicy: .reloadIgnoringLocalCacheData, emptyHeaders: true)

    /// Held so `serviceExtensionTimeWillExpire` can still answer: the system
    /// kills this process if nothing calls the handler, and a killed extension
    /// delivers the original push anyway — but only after making the human wait
    /// for the timeout. All three are read and written under `lock` only.
    private let lock = NSLock()
    private var handler: ((UNNotificationContent) -> Void)?
    private var original: UNNotificationContent?
    private var work: Task<Void, Never>?

    override func didReceive(
        _ request: UNNotificationRequest,
        withContentHandler contentHandler: @escaping (UNNotificationContent) -> Void
    ) {
        lock.withLock {
            handler = contentHandler
            original = request.content
        }

        // An agent's draft is not an event: a different fetch, a different
        // banner, and no badge (a draft changes nothing the badge counts).
        if let draft = EventBanner.draftPush(request.content.userInfo[PushRoute.eventIdKey] as? String) {
            nonisolated(unsafe) let fallback = request.content
            let task = Task { [weak self] in
                let content = await Self.draftContent(draft)
                guard let self else { return }
                self.deliver(content ?? fallback)
            }
            let spent = lock.withLock { () -> Bool in
                work = task
                return handler == nil
            }
            if spent { task.cancel() }
            return
        }

        // An event reminder: its own fetch and banner, no badge (an event
        // changes nothing the badge counts).
        if let reminder = EventBanner.calendarPush(request.content.userInfo[PushRoute.eventIdKey] as? String) {
            nonisolated(unsafe) let fallback = request.content
            let task = Task { [weak self] in
                let content = await Self.calendarContent(reminder)
                guard let self else { return }
                self.deliver(content ?? fallback)
            }
            let spent = lock.withLock { () -> Bool in
                work = task
                return handler == nil
            }
            if spent { task.cancel() }
            return
        }

        guard let route = PushRoute(userInfo: request.content.userInfo) else {
            return deliver(request.content)
        }
        // This request's own content, for the no-event fallback below. Not
        // read back from `original`, which a later request on the same
        // instance would have replaced. Unsafe only to the checker: the
        // content is an immutable snapshot and nothing here mutates it.
        nonisolated(unsafe) let fallback = request.content
        let task = Task { [weak self] in
            // BOTH CALLS AT ONCE, because there is one timeout for the pair.
            // Both fetches are STATIC: neither needs the instance, and a child
            // task that captured it would have nothing to gain. Each is a
            // keychain read, a request and a decode.
            // The banner's own fetch and the badge's are independent questions
            // to the same daemon, and running them back to back would spend two
            // round trips out of the seconds this process gets — enough, on a
            // slow link, to lose the enrichment that is the whole point of the
            // extension in order to add a number to an icon.
            async let banner = Self.fetch(route)
            async let due = Self.dueToday(route)
            let (event, count) = await (banner, due)
            guard let self else { return }
            guard let event else {
                return self.deliver(Self.badged(fallback, count: count))
            }
            // Badged BEFORE the sender is attached, so the communication
            // content `updating(from:)` returns is the one delivered as is.
            let base = Self.badged(Self.content(for: event, accountId: route.accountId), count: count)
            self.deliver(await Self.withSender(base, event: event))
        }
        // Published after launch, so an expiry in between could not cancel
        // it: if the handler is already spent, cancel the fetch here instead.
        let spent = lock.withLock { () -> Bool in
            work = task
            return handler == nil
        }
        if spent { task.cancel() }
    }

    /// The app icon's number, stamped onto whatever banner is about to go out.
    ///
    /// A NIL COUNT LEAVES THE BADGE ALONE, which is why this takes an optional
    /// rather than defaulting to zero: `badge = 0` CLEARS the icon, so a failed
    /// or timed-out fetch that fell back to zero would tell someone with four
    /// overdue bills that they were clear. Not knowing and knowing it is nothing
    /// are different answers and the payload can say both.
    private static func badged(_ content: UNNotificationContent, count: Int?)
        -> UNNotificationContent
    {
        guard let count,
            let mutable = content.mutableCopy() as? UNMutableNotificationContent
        else { return content }
        mutable.badge = NSNumber(value: count)
        return mutable
    }

    /// How many rows the For-your-eyes band holds, for the mailbox this push
    /// came from: the same `NeedToday.count` the app's own badge writer uses,
    /// over the same standing band.
    ///
    /// `peek=true` IS LOAD-BEARING AND NOT AN OPTIMIZATION. A plain read of this
    /// band STAMPS every row it returns as surfaced (see the daemon's updates
    /// handler: peek's only effect is skipping that ledger write), which would
    /// promote `new` to `open` and empty the new band. Refreshing a number must
    /// not be able to mark mail as seen — least of all from a background process
    /// the user never opened.
    ///
    /// Counted HERE rather than asked for: the daemon serves no due-today
    /// figure, and inventing one server-side would be a second definition of
    /// "today" evaluated in a second timezone. The rows come back and the shared
    /// pure function decides, exactly as it does in the app.
    private static func dueToday(_ route: PushRoute) async -> Int? {
        guard let settings = try? SettingsStore.load(accountId: route.accountId) else { return nil }

        var base = settings.serverURL.trimmingCharacters(in: .whitespacesAndNewlines)
        while base.hasSuffix("/") { base.removeLast() }
        guard var comps = URLComponents(string: base + "/client/v2/feed") else { return nil }
        comps.queryItems = [
            URLQueryItem(name: "destination", value: "fye"),
            // THE SAME PAGE THE APP READS. This route defaults to fifty rows
            // when nobody says otherwise, and the poller asks for
            // NeedToday.bandLimit — so omitting it here counted a smaller set
            // than the headline the badge is meant to agree with, and under-
            // reported, because this band is ordered by importance and the rows
            // past a cut are not the least due ones.
            URLQueryItem(name: "limit", value: String(NeedToday.bandLimit)),
        ]
        guard let url = comps.url else { return nil }

        var request = URLRequest(url: url, timeoutInterval: 4)
        request.setValue("Bearer \(settings.apiToken)", forHTTPHeaderField: "Authorization")
        request.setValue("application/json", forHTTPHeaderField: "Accept")

        // Silent on every failure, like the fetch above and for the same reason:
        // the URL names the user's daemon and the header held a capability. A
        // miss here costs a stale badge, which the app corrects the moment it is
        // opened.
        guard let (data, response) = try? await Self.countSession.data(for: request),
            (response as? HTTPURLResponse)?.statusCode == 200,
            let page = try? JSONDecoder().decode(AgentFeed.self, from: data)
        else { return nil }
        return page.total_count ?? page.items.count
    }

    /// Out of time. Whatever the fetch was doing, the original alert goes out
    /// now — this is the fallback the relay's generic copy exists for.
    override func serviceExtensionTimeWillExpire() {
        let (task, original) = lock.withLock { (work, self.original) }
        task?.cancel()
        if let original { deliver(original) }
    }

    /// Call the system's handler at most once. Both the fetch and the expiry
    /// path can reach here, possibly on different threads, and the second call
    /// is a crash in some OS versions. The handler is taken under the lock and
    /// called outside it.
    private func deliver(_ content: UNNotificationContent) {
        let handler = lock.withLock { () -> ((UNNotificationContent) -> Void)? in
            defer { self.handler = nil }
            return self.handler
        }
        handler?(content)
    }

    /// Ask THIS account's daemon what the event was. The bearer comes from the
    /// same per-account keychain slots the app writes, which is the whole
    /// reason the extension shares its keychain access group.
    private static func fetch(_ route: PushRoute) async -> Event? {
        guard let settings = try? SettingsStore.load(accountId: route.accountId) else { return nil }

        var base = settings.serverURL.trimmingCharacters(in: .whitespacesAndNewlines)
        while base.hasSuffix("/") { base.removeLast() }
        guard let url = URL(string: "\(base)/client/events/\(route.eventId)") else { return nil }

        var request = URLRequest(url: url, timeoutInterval: 10)
        request.setValue("Bearer \(settings.apiToken)", forHTTPHeaderField: "Authorization")
        request.setValue("application/json", forHTTPHeaderField: "Accept")

        // A 404 is the daemon's answer for "not yours or not there" and is a
        // fallback, not an error worth distinguishing. Nothing about the
        // failure is logged: the URL names the user's daemon and the header
        // held a capability.
        guard let (data, response) = try? await Self.session.data(for: request),
            (response as? HTTPURLResponse)?.statusCode == 200
        else { return nil }
        return try? JSONDecoder().decode(Event.self, from: data)
    }

    /// The banner the app itself would have posted for this event, built from
    /// the same mapping so a push and a live frame are indistinguishable.
    private static func content(for event: Event, accountId: UUID) -> UNNotificationContent {
        switch EventBanner.routing(for: event) {
        case .threadBanner: return threadContent(for: event, accountId: accountId)
        case .authSignal(let kind): return authContent(kind: kind, event: event, accountId: accountId)
        }
    }

    /// An ordinary mail banner re-dressed as a communication notification, so
    /// the sender's avatar leads and Passband's icon is the badge (see
    /// SenderAvatar). Not for auth banners, which lead with the kind of event
    /// rather than who sent it, and not for a read receipt, whose "sender" is
    /// the account's own address.
    private static func withSender(_ content: UNNotificationContent, event: Event) async
        -> UNNotificationContent
    {
        guard case .threadBanner = EventBanner.routing(for: event), event.kind != .opened,
            !event.sender.isEmpty
        else { return content }
        return await SenderAvatar.communication(content, sender: event.sender, name: content.title)
    }

    /// AUTH MAIL. The phone cannot poll in the background, so this push is the
    /// only way a login code reaches a phone at all — and it is the one banner
    /// here that is NOT built from what the mail says. Two fields of the event
    /// are read, the kind and the from address, and both are metadata of the
    /// class `/client/sealed` already serves; `one_line` is not read, because a
    /// subject is so often the code itself and a lock screen is not a place to
    /// leave a credential. (The daemon writes a fixed per-kind phrase into that
    /// field for exactly this row, but the rule here does not lean on it.)
    ///
    /// The route marker is what keeps the tap off a thread fetch: a sealed
    /// thread is one the daemon refuses to serve, so an ordinary event banner
    /// would open the app onto nothing. `handleAuthTap` lands on the Auth list,
    /// where asking for the code is the human's own audited act.
    ///
    /// No account name: this process reads credentials out of the shared
    /// keychain and has no access to the app's account labels, so the kind
    /// stands alone. `EventBanner.authCopy` takes nil for that and says so.
    private static func authContent(
        kind: SealedKind, event: Event, accountId: UUID
    ) -> UNNotificationContent {
        let account = accountId.uuidString
        let copy = EventBanner.authCopy(kind: kind, sender: event.sender, accountName: nil)
        let content = UNMutableNotificationContent()
        content.title = copy.title
        content.body = copy.body
        content.threadIdentifier = "\(account).\(copy.threadIdentifier)"
        content.userInfo = [
            EventBanner.accountKey: account,
            EventBanner.routeKey: EventBanner.authRoute,
            EventBanner.messageKey: event.message_id,
            EventBanner.authKey: event.isAuth || event.sealed_kind != nil,
            EventBanner.threadKey: event.thread_id,
            EventBanner.senderKey: event.sender,
        ]
        // Always a chime, and the system's rather than the user's chosen one:
        // the app's sound preference lives in ITS UserDefaults, which this
        // process does not share.
        content.sound = copy.sound ? .default : nil
        return content
    }

    /// The ordinary event's banner: who the mail is from, what triage made of
    /// it, and a tap that opens the thread.
    private static func threadContent(for event: Event, accountId: UUID)
        -> UNNotificationContent
    {
        let copy = EventBanner.copy(for: event)
        let account = accountId.uuidString
        let content = UNMutableNotificationContent()
        content.title = copy.title
        content.body = copy.body
        // Account-prefixed for the reason `Notifier` prefixes its own: thread
        // ids are per-daemon, so two mailboxes would otherwise stack unrelated
        // mail into one conversation.
        content.threadIdentifier = "\(account).\(copy.threadIdentifier)"
        // The keys a tap routes on, written exactly as the live path writes
        // them — `NotificationDelegate` cannot tell the two apart, and must not.
        content.userInfo = [
            EventBanner.threadKey: event.thread_id,
            EventBanner.eventKey: event.id,
            EventBanner.messageKey: event.message_id,
            EventBanner.authKey: event.isAuth || event.sealed_kind != nil,
            EventBanner.accountKey: account,
        ]
        if event.isAuth || event.sealed_kind != nil {
            content.userInfo[EventBanner.senderKey] = event.sender
        }
        // The system default rather than the user's chosen chime: the app's
        // sound preference lives in ITS UserDefaults, which this process does
        // not share. Silencing the kinds the app silences matters more than
        // which chime the rest use.
        content.sound = copy.sound ? .default : nil
        return content
    }

    // MARK: - an event reminder

    /// The banner the Mac would post for this reminder (CalendarReminder.swift),
    /// or nil to keep the relay's "An event is coming up". The push names a
    /// fire by id; what is shown is what THIS account's daemon serves under
    /// it, and only when the ids agree.
    ///
    /// The raw push's `event_id` survives a nil here, so a tap on the generic
    /// banner still opens the reminder (`EventBanner.tapRoute`).
    private static func calendarContent(_ push: EventBanner.CalendarPush) async -> UNNotificationContent? {
        guard let settings = try? SettingsStore.load(accountId: push.accountId) else { return nil }

        var base = settings.serverURL.trimmingCharacters(in: .whitespacesAndNewlines)
        while base.hasSuffix("/") { base.removeLast() }
        guard let url = URL(string: base + CalendarReminderSignal.path(push.fireId)) else { return nil }

        var request = URLRequest(url: url, timeoutInterval: 10)
        request.setValue("Bearer \(settings.apiToken)", forHTTPHeaderField: "Authorization")
        request.setValue("application/json", forHTTPHeaderField: "Accept")

        // Silent on every failure, for the reason `fetch` is.
        guard let (data, response) = try? await Self.session.data(for: request),
            (response as? HTTPURLResponse)?.statusCode == 200,
            let signal = CalendarReminderSignal.decode(data),
            signal.calendar_reminder == push.fireId
        else { return nil }

        let account = push.accountId.uuidString
        let copy = EventBanner.calendarReminderCopy(signal)
        let content = UNMutableNotificationContent()
        content.title = copy.title
        content.subtitle = copy.subtitle
        content.body = copy.body
        content.threadIdentifier = "\(account).\(copy.threadIdentifier)"
        content.userInfo = EventBanner.calendarReminderUserInfo(signal, account: push.accountId)
        // The app registers the category and its "Show tickets" action.
        if signal.ticket != nil { content.categoryIdentifier = EventBanner.calendarTicketCategory }
        // The system's chime: the app's choice lives in ITS defaults.
        content.sound = copy.sound ? .default : nil
        return content
    }

    // MARK: - an agent's draft

    /// The banner for a draft an agent asked the owner to review, or nil to
    /// keep the relay's generic one. The push names a draft by id and creation
    /// second only; what is shown is whatever THIS account's daemon holds
    /// under that id, and only when the daemon marks it as an agent's and its
    /// second matches. The owner's own drafts never become a banner.
    ///
    /// The raw push's `event_id` survives a nil here, so a tap on the generic
    /// banner still opens the draft (`EventBanner.tapRoute`).
    private static func draftContent(_ push: EventBanner.DraftPush) async -> UNNotificationContent? {
        guard let settings = try? SettingsStore.load(accountId: push.accountId) else { return nil }

        var base = settings.serverURL.trimmingCharacters(in: .whitespacesAndNewlines)
        while base.hasSuffix("/") { base.removeLast() }
        guard var comps = URLComponents(string: base + "/client/drafts") else { return nil }
        comps.queryItems = [URLQueryItem(name: "forwards", value: "1")]
        guard let url = comps.url else { return nil }

        var request = URLRequest(url: url, timeoutInterval: 10)
        request.setValue("Bearer \(settings.apiToken)", forHTTPHeaderField: "Authorization")
        request.setValue("application/json", forHTTPHeaderField: "Accept")

        // Silent on every failure, for the reason `fetch` is.
        guard let (data, response) = try? await Self.session.data(for: request),
            (response as? HTTPURLResponse)?.statusCode == 200,
            let rows = try? JSONDecoder().decode([DraftView].self, from: data),
            let draft = rows.first(where: { $0.id == push.draftId }),
            draft.from_agent == true,
            EventBanner.draftSecond(draft.created_at) == push.created
        else { return nil }

        let account = push.accountId.uuidString
        let copy = EventBanner.agentDraftCopy(
            to: draft.to, subject: draft.subject, isReply: draft.reply_to_message_id != nil,
            parentFrom: draft.parent_from)
        let content = UNMutableNotificationContent()
        content.title = copy.title
        content.body = copy.body
        content.threadIdentifier = "\(account).agent-drafts"
        content.userInfo = [
            EventBanner.routeKey: EventBanner.agentDraftRoute,
            EventBanner.accountKey: account,
            EventBanner.draftKey: push.draftId,
            EventBanner.draftCreatedKey: push.created,
        ]
        content.sound = .default
        return content
    }
}
