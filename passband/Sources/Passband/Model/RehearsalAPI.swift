import Foundation

/// An in-memory implementation of the human-door wire contract. The production
/// API client, dashboard, reader, actions, and rules editor all use this transport
/// in rehearsal. Unsupported operations fail here; none fall through to a server.
actor RehearsalAPI {
    static let shared = RehearsalAPI()
    private var mailbox = OnboardingRehearsal()
    private var referenceDate: Date
    private var rules: [SenderRule] = []
    private var nextRuleID = 1
    /// Send groups, held WITH their membership; the list read strips it, the
    /// way the daemon's does. Empty in the lesson, which never makes one: they
    /// are here so the Groups page and its editor answer in rehearsal instead
    /// of failing a save that was never going anywhere.
    private var groups: [SendGroup] = []
    private var nextGroupID = 1
    /// What a staged group has "sent". Nothing in rehearsal sends, so this is
    /// only ever what `stage` put there.
    private var groupHistory: [Int: [GroupHistoryEntry]] = [:]
    /// The drafts the practice composer saved, kept for the session only so
    /// the picker is not empty right after a save. `reset` drops them, so a
    /// practice draft never outlives the rehearsal.
    private var drafts: [Int: DraftView] = [:]
    private var nextDraftID = 1
    private var clearedShipments: Set<Int> = []
    /// The practice "Languages I read", kept for the session so the card's
    /// add and remove answer the way a daemon's do. Nothing in practice is
    /// translated by it; `reset` drops it.
    private var readLanguages: [String] = []
    private var statusOverrides: [Int: AttentionStatus] = [:]
    private var tierOverrides: [Int: Tier] = [:]
    private var importanceOverrides: [Int: Int] = [:]
    private var reminders: [Int: String] = [:]
    private var newsletterPhotos: [String: String] = [:]
    /// The live mailbox's address, when practice was entered from one: the
    /// greeting seeds its name from `/client/stats`, and a fixture address
    /// there would name the person after the fixture.
    private var accountEmail: String?
    /// Where the newsletter photographs live when there is no app bundle to
    /// ask — the test binary's checkout. The app passes nothing and reads
    /// its bundle; a shipped binary carries no path into anyone's sources.
    private let resourceDirectory: URL?

    init(referenceDate: Date = Date(), resourceDirectory: URL? = nil) {
        self.referenceDate = referenceDate
        self.resourceDirectory = resourceDirectory
    }

    func reset(accountEmail: String? = nil) {
        mailbox.reset()
        self.accountEmail = accountEmail
        referenceDate = Date()
        rules = []; nextRuleID = 1; drafts = [:]; nextDraftID = 1; clearedShipments = []
        groups = []; nextGroupID = 1; groupHistory = [:]; readLanguages = []
        statusOverrides = [:]; tierOverrides = [:]; importanceOverrides = [:]; reminders = [:]
    }
    func snapshot() -> OnboardingRehearsal { mailbox }

    #if DEBUG
        /// Rules and groups laid in for a screenshot (PassbandiOS/Lib/
        /// FixtureStage.swift). The lesson never calls this: a practice inbox
        /// that arrived with rules already written would be teaching someone
        /// to tune senders that were tuned for them.
        func stage(rules: [SenderRule], groups: [SendGroup], history: [Int: [GroupHistoryEntry]]) {
            self.rules = rules
            nextRuleID = (rules.map(\.id).max() ?? 0) + 1
            self.groups = groups
            nextGroupID = (groups.map(\.id).max() ?? 0) + 1
            groupHistory = history
        }

        /// THREADS IN ANOTHER LANGUAGE, which the lesson does not have: each
        /// is a run of versions served one per fetch, the last one sticking, so
        /// a pending translation can be seen to land the way a daemon's does.
        /// A version carrying a `deferred` translation is the daemon's archive
        /// row: it STAYS at the front however often it is read, and only the
        /// reader's `POST .../translate` moves past it, so a prefetch can never
        /// look like an ask. See `TranslationFixture`.
        private var stagedThreads: [String: [ClientThreadView]] = [:]
        func stage(thread versions: [ClientThreadView]) {
            guard let id = versions.first?.thread_id else { return }
            stagedThreads[id] = versions
        }
        private static func isDeferred(_ view: ClientThreadView) -> Bool {
            view.messages.contains { $0.translation?.status == .deferred }
        }
        private func stagedThread(_ id: String) -> ClientThreadView? {
            guard var versions = stagedThreads[id], let next = versions.first else { return nil }
            if versions.count > 1, !Self.isDeferred(next) { versions.removeFirst() }
            stagedThreads[id] = versions
            return next
        }
        /// The daemon's promote: the deferred version gives way to the next.
        /// How many it moved, as the daemon's `promoted` would count them.
        private func translateStaged(_ id: String) -> Int {
            guard var versions = stagedThreads[id], let next = versions.first,
                versions.count > 1, Self.isDeferred(next)
            else { return 0 }
            versions.removeFirst()
            stagedThreads[id] = versions
            return next.messages.filter { $0.translation?.status == .deferred }.count
        }

        /// Sent mail and an outbox, for the same reason and the same caller:
        /// the lesson sends nothing and schedules nothing, so its Sent page is
        /// the empty one. Only a screenshot ever fills these.
        private var stagedSent: [SentItem] = []
        private var stagedScheduled: [ScheduledSendView] = []
        func stage(sent: [SentItem], scheduled: [ScheduledSendView]) {
            stagedSent = sent
            stagedScheduled = scheduled
        }

        /// A timeline, and the stats that go with it (a granted Google
        /// Calendar, so no connect card). The lesson never claims one; only the phone's
        /// screenshot harness stages this (`--fixture-calendar`).
        private var stagedTimeline: TimelineFeed?
        func stage(timeline: TimelineFeed) {
            stagedTimeline = timeline
        }
    #endif

    private func withStagedCalendar(_ stats: StoreStats) -> StoreStats {
        #if DEBUG
            guard stagedTimeline != nil else { return stats }
            var out = stats
            out.calendar = CalendarAccess(granted: true)
            return out
        #else
            return stats
        #endif
    }

    /// Confirm the saved rule, not the editor's requested/default disposition,
    /// actually governs and suppresses this sample after specificity is resolved.
    func suppresses(messageID: Int, viaRuleID: Int) -> Bool {
        guard let message = mailbox.messages.first(where: { $0.id == messageID }),
              let governing = ruleFor(message), governing.id == viaRuleID,
              governing.disposition == .squelch else { return false }
        return update(message).tier == .noise
    }

    func response(for request: URLRequest) throws -> (Data, HTTPURLResponse) {
        guard let url = request.url else { throw APIError(.badRequest, 400, "Missing practice request URL") }
        let path = url.path
        let method = request.httpMethod ?? "GET"
        let components = path.split(separator: "/").map(String.init)
        let query = Dictionary((URLComponents(url: url, resolvingAgainstBaseURL: false)?.queryItems ?? []).map { ($0.name, $0.value ?? "") }, uniquingKeysWith: { _, new in new })
        let body = (request.httpBody.flatMap { try? JSONSerialization.jsonObject(with: $0) }) as? [String: Any] ?? [:]
        let data: Data

        #if DEBUG
            if method == "GET", components.count == 3, components[1] == "thread",
                let staged = stagedThread(components[2])
            {
                return (
                    try encode(staged),
                    HTTPURLResponse(
                        url: url, statusCode: 200, httpVersion: "HTTP/1.1",
                        headerFields: ["Content-Type": "application/json"])!
                )
            }
            if method == "POST", components.count == 4, components[1] == "thread",
                components[3] == "translate", stagedThreads[components[2]] != nil
            {
                return (
                    try json(["promoted": translateStaged(components[2])]),
                    HTTPURLResponse(
                        url: url, statusCode: 200, httpVersion: "HTTP/1.1",
                        headerFields: ["Content-Type": "application/json"])!
                )
            }
        #endif

        switch (method, path) {
        case ("GET", "/client/v2/feed"):
            let destination = query["destination"] ?? "fye"
            let items = mailbox.messages.filter { message in
                let row = update(message)
                switch destination {
                case "fye": return row.status != .done && row.tier != .noise
                case "reading": return message.lane == .reading && !message.isDone && ruleFor(message)?.disposition != .squelch
                case "records": return message.lane == .records
                default: return false
                }
            }.map { message in
                let row = update(message)
                let kinds: [OnboardingRehearsal.Category: String] = [
                    .attention: "correspondence", .reading: "editorial", .calendar: "event_reservation",
                    .shipments: "delivery", .banking: "financial_update", .receipts: "receipt"]
                return AgentFeedItem(message_id: message.id, thread_id: threadID(message.id),
                    from_addr: address(message), subject: message.subject, received_at: stamp(),
                    decision: AgentMessageDecision(kinds: [kinds[message.category]!],
                        destinations: message.lane == .reading ? ["reading"] : [],
                        // The club night (id 6) is an event promotion, so the
                        // practice board shows the content/promotion split.
                        reading_kind: message.lane == .reading
                            ? (message.id == 6 ? "promotion" : "content") : nil,
                        summary: message.explanation, reason: message.explanation, records: recordFacts(message)),
                    attention: AgentAttention(show_in_fye: row.status != .done && row.tier != .noise,
                        state: "informational", summary: message.explanation,
                        factors: AgentAttentionFactors(urgency: 0, action_need: 0, personal_relevance: 1, importance: 0.5)), score: 0)
            }
            data = try encode(AgentFeed(total_count: items.count, version: 2, ranked_at: stamp(),
                items: Array(items.prefix(max(1, query["limit"].flatMap(Int.init) ?? 200)))))
        case ("GET", "/client/stats"):
            let updates = allUpdates()
            let stats = StoreStats(
                tier_counts: Dictionary(grouping: updates, by: { $0.tier.rawValue }).mapValues(\.count),
                account_email: accountEmail, total: mailbox.messages.count, sealed: 0,
                spam: 0,
                bands: BandCounts(standing: bandRows("standing").count, new: bandRows("new").count, open: bandRows("open").count),
                last_surfaced_at: stamp(),
                inbox_unread: InboxUnread(messages: mailbox.messages.filter { !$0.isRead }.count, threads: Set(mailbox.messages.filter { !$0.isRead }.map { threadID($0.id) }).count),
                assistant_relay: false, invite_sharing: false, forwarding: false,
                gmail: GmailHealth(connected: true),
                // Without it the saver falls back to the key-less PUT, which
                // is not stubbed here: every practice save would fail
                // silently under a "kept in drafts" toast.
                drafts_v2: true)
            data = try encode(withStagedCalendar(stats))
        case ("GET", "/client/updates"):
            var rows = query["band"].map { bandRows($0) } ?? allUpdates()
            if let tier = query["tier"] { rows = rows.filter { $0.tier.rawValue == tier } }
            if let status = query["status"] { rows = rows.filter { $0.status.rawValue == status } }
            if let min = query["min_importance"].flatMap(Int.init) { rows = rows.filter { $0.importance >= min } }
            if query["spam"] == "only" { rows = [] }
            if query["reminders"] == "pending" { rows = rows.filter { $0.remind_at != nil } }
            let offset = max(0, query["cursor"].flatMap(Int.init) ?? 0)
            let limit = max(1, query["limit"].flatMap(Int.init) ?? 200)
            data = try encode(Page(items: Array(rows.dropFirst(offset).prefix(limit)), next_cursor: offset + limit < rows.count ? String(offset + limit) : nil))
        case ("GET", "/client/shipments"):
            let names = [5: "Canvas weekend bag", 13: "Linen notebooks", 14: "Windowsill herb planter"]
            let rows = mailbox.messages.filter { $0.category == .shipments && !clearedShipments.contains($0.id) }.map {
                Shipment(id: $0.id, account_id: 1, tracking_number: "PRACTICE-\($0.id)", carrier: .unknown, item_name: names[$0.id]!, status: $0.id == 13 ? .outForDelivery : .shipped, thread_id: threadID($0.id), first_seen: stamp(-86400), last_update: stamp(), eta: stamp($0.id == 13 ? 3600 : $0.id == 5 ? 86400 : 172800))
            }
            data = try encode(rows)
        case ("GET", "/client/receipts"):
            let amounts = [2: 8.50, 8: 4.75, 19: 24.00]
            let rows = mailbox.messages.filter { $0.category == .receipts }.map {
                Receipt(id: $0.id, account_id: 1, message_id: $0.id, thread_id: threadID($0.id), from_addr: address($0), from_name: $0.sender, amount: amounts[$0.id], currency: "USD", received_at: stamp())
            }
            data = try encode(rows)
        case ("GET", "/client/calendar"):
            let titles = [10: "Dinner at Juniper Table", 17: "Design review with Maya · Zip", 18: "Nora Vale · The Atlas of Small Things"]
            let rows = mailbox.messages.filter { $0.category == .calendar }.map {
                CalendarUpdate(id: $0.id, message_id: $0.id, thread_id: threadID($0.id), kind: $0.id == 17 ? .invite : .reservation, event_title: titles[$0.id], starts_at: eventDate($0.id).ISO8601Format(), organizer: $0.id == 17 ? "Maya Chen" : $0.sender, received_at: stamp())
            }
            data = try encode(rows)
        case ("GET", "/client/banking"):
            let amounts = [9: 1240.50, 15: 86.20, 16: 250.00]
            let hints = [9: "2048", 15: "5310", 16: "7712"]
            let rows = mailbox.messages.filter { $0.category == .banking }.map {
                BankingRecord(id: $0.id, message_id: $0.id, thread_id: threadID($0.id), from_addr: address($0), kind: $0.id == 9 ? .statement : $0.id == 15 ? .autopay : .transactionAlert, institution: $0.sender, amount: amounts[$0.id], currency: "USD", account_hint: hints[$0.id], received_at: stamp())
            }
            data = try encode(rows)
        case ("GET", "/client/rules"):
            data = try encode(rules)
        case ("POST", "/client/rules"):
            data = try saveRule(body, id: nil)
        case ("GET", "/client/rules/suggest"):
            // No model in practice: the card shows its generic example.
            data = try encode(RuleSuggestion(suggestion: nil))
        case ("GET", "/client/search"):
            let term = (query["q"] ?? "").lowercased()
            let hits = mailbox.messages.filter { message in
                (term.isEmpty || "\(message.sender) \(message.subject) \(message.body)".lowercased().contains(term))
            }.map { SearchHit(id: $0.id, thread_id: threadID($0.id), from_addr: address($0), from_name: $0.sender, subject: $0.subject, received_at: stamp(), snippet: $0.explanation, legs: ["keyword"]) }
            data = try encode(SearchPage(items: hits, match_kind: "keyword", sort: query["sort"]))
        case ("GET", "/client/groups"):
            // Counts only, like the daemon's listing; `q` is the composer's
            // autocomplete and matches on the name.
            let term = query["q"]?.lowercased()
            let listed = groups.filter { term.map($0.name.lowercased().contains) ?? true }
                .sorted { $0.name.lowercased() < $1.name.lowercased() }
                .map { group in var row = group; row.members = []; return row }
            data = try encode(listed)
        case ("POST", "/client/groups"):
            data = try saveGroup(body, id: nil)
        case ("GET", "/client/groups/limits"):
            data = try encode(GroupLimits(max_members: 50))
        case ("GET", "/client/sealed"), ("GET", "/client/marketing"), ("GET", "/client/unsubscribes"), ("GET", "/client/audit"), ("GET", "/client/triage-feedback"), ("GET", "/client/contacts"), ("GET", "/client/senders"), ("GET", "/client/sender-domains"):
            data = Data("[]".utf8)
        case ("GET", "/client/person"):
            // The practice inbox has no history behind its senders: the card
            // shows who they are and nothing else.
            data = try json([
                "addr": query["addr"] ?? "", "is_self": false, "known_contact": false,
                "groups": [Any](), "recent_threads": [Any](),
            ])
        case ("GET", "/client/drafts"):
            // Newest first, like the daemon. `forwards=1` is not consulted:
            // there is no older app in a rehearsal to hide forwards from.
            data = try encode(drafts.values.sorted { $0.id > $1.id })
        case ("POST", "/client/drafts"):
            // The draft routes, stubbed so a practice composer's autosave
            // answers like a daemon's rather than failing (see `drafts`).
            data = try practiceDraft(body, id: nil)
        case ("PUT", "/client/drafts"):
            // The keyed upsert replies and forwards use. Key-less is the old
            // new-message slot, which a `drafts_v2` client never sends.
            let reply = body["reply_to_message_id"] as? Int
            let forward = body["forward_of_message_id"] as? Int
            guard reply != nil || forward != nil else { throw unsupported() }
            let existing = drafts.values.first {
                $0.reply_to_message_id == reply && $0.forward_of_message_id == forward
            }
            data = try practiceDraft(body, id: existing?.id)
        #if DEBUG
            // Staged rows only (see `stage(sent:scheduled:)`); unstaged, the
            // Sent page answers exactly what it does in a shipped build.
            case ("GET", "/client/sent") where !stagedSent.isEmpty:
                data = try encode(["items": stagedSent])
            case ("GET", "/client/scheduled") where !stagedScheduled.isEmpty:
                data = try encode(stagedScheduled)
            case ("GET", "/client/timeline") where stagedTimeline != nil:
                data = try encode(stagedTimeline)
        #endif
        case ("GET", "/client/sent"):
            data = try json(["items": []])
        // The practice stats never claim a timeline, so the calendar page is
        // not reachable here; these answer anyway, empty, so nothing that
        // asks can crash the lesson.
        case ("GET", "/client/timeline"):
            data = try json(["ghosts": []])
        case ("POST", "/client/timeline/refresh"):
            data = try json(["triggered": false])
        case ("GET", "/client/invites"):
            data = try json(["can_share": false])
        case ("GET", "/client/tracking-config"):
            data = try json(["available": false, "default_enabled": false])
        case ("GET", "/client/translate-config"):
            data = try translateConfig()
        case ("POST", "/client/translate-config"):
            // The daemon's rule: unknown codes and English are dropped, and the
            // body replaces the whole set.
            let known = Set(Self.practiceLanguages.map(\.code))
            var next: [String] = []
            for code in (body["read_languages"] as? [String] ?? []).map({ $0.lowercased() })
            where known.contains(code) && !next.contains(code) {
                next.append(code)
            }
            readLanguages = next
            data = try translateConfig()
        case ("POST", "/client/refresh"), ("POST", "/client/spam/refresh"):
            // The sample inbox is already populated; refresh reads its current state.
            data = try json(["triggered": true])
        case ("POST", "/client/triage-feedback"):
            guard let id = body["message_id"] as? Int, has(id), let value = body["to_value"] as? String else { throw missing() }
            switch body["dimension"] as? String {
            case "tier": tierOverrides[id] = Tier(rawValue: value)
            case "importance": importanceOverrides[id] = Int(value)
            default: throw unsupported()
            }
            data = try json(["ok": true])
        case ("POST", "/client/actions/archive"):
            guard let id = body["message_id"] as? Int, has(id) else { throw missing() }
            setStatus(id, .done)
            data = try encode(StatusResult(status: "done", message_id: id))
        default:
            if components.count >= 4, components[1] == "v2", components[2] == "messages",
               let id = Int(components[3]), has(id) {
                if components.count == 5, components[4] == "opened", method == "POST" {
                    mailbox.open(id)
                    data = try json(["ok": true])
                } else if components.count == 4, method == "GET" {
                    let nested = URLRequest(url: URL(string: "https://rehearsal.invalid/client/thread/\(threadID(id))")!)
                    let (threadData, _) = try response(for: nested)
                    let thread = try JSONDecoder().decode(ClientThreadView.self, from: threadData)
                    data = try encode(HumanMessageEnvelope(message_id: id, thread: thread))
                } else { throw unsupported() }
            } else if components.count == 3, components[1] == "thread", method == "GET" {
                let members = mailbox.messages.filter { threadID($0.id) == components[2] }
                guard let first = members.first else { throw missing() }
                let messages = members.map { message in
                    let update = update(message)
                    return ClientMessage(id: message.id, from_addr: address(message), from_name: message.sender, received_at: stamp(Double(message.id)), content: renderedBody(message), html: renderedHTML(message), subject: message.subject, attachments: [], is_sent: false, is_spam: false, tier: update.tier, deadline: update.deadline, attention_open: update.status != .done, one_line: update.one_line, sender_known: message.lane == .attention)
                }
                data = try encode(ClientThreadView(thread_id: components[2], subject: first.subject, messages: messages))
            } else if components.count == 4, components[1] == "thread", components[3] == "opened", method == "POST" {
                for message in mailbox.messages where threadID(message.id) == components[2] { mailbox.open(message.id) }
                data = try json(["ok": true])
            } else if components.count == 4, components[1] == "thread", components[3] == "translate", method == "POST" {
                // The practice inbox is English: nothing is deferred, nothing moves.
                data = try json(["promoted": 0])
            } else if components.count == 4, components[1] == "updates", let id = Int(components[2]), has(id) {
                if components[3] == "status", method == "POST", let raw = body["status"] as? String, let status = AttentionStatus(rawValue: raw) {
                    setStatus(id, status)
                    data = try encode(StatusResult(status: raw, message_id: id))
                } else if components[3] == "reminder", method == "POST", let stamp = body["remind_at"] as? String {
                    reminders[id] = stamp; setStatus(id, .done)
                    data = try json(["message_id": id, "remind_at": stamp])
                } else if components[3] == "reminder", method == "DELETE" {
                    reminders[id] = nil
                    data = try json(["ok": true])
                } else { throw unsupported() }
            } else if components.count == 3, components[1] == "drafts", method == "PUT",
                      let id = Int(components[2]) {
                guard drafts[id] != nil else { throw missing() }
                data = try practiceDraft(body, id: id)
            } else if components.count == 3, components[1] == "drafts", method == "DELETE",
                      let id = Int(components[2]) {
                guard drafts.removeValue(forKey: id) != nil else { throw missing() }
                data = Data()
            } else if components.count == 3, components[1] == "rules", let id = Int(components[2]) {
                guard rules.contains(where: { $0.id == id }) else { throw missing() }
                if method == "PUT" { data = try saveRule(body, id: id) }
                else if method == "DELETE" { rules.removeAll { $0.id == id }; data = try json(["deleted": true]) }
                else { throw unsupported() }
            } else if components.count == 3, components[1] == "groups", let id = Int(components[2]) {
                guard let group = groups.first(where: { $0.id == id }) else { throw missing() }
                if method == "GET" { data = try encode(group) }
                else if method == "PUT" { data = try saveGroup(body, id: id) }
                else if method == "DELETE" {
                    groups.removeAll { $0.id == id }; groupHistory[id] = nil
                    data = try json(["deleted": true])
                } else { throw unsupported() }
            } else if components.count == 4, components[1] == "groups", components[3] == "history", method == "GET", let id = Int(components[2]) {
                guard groups.contains(where: { $0.id == id }) else { throw missing() }
                data = try encode(GroupHistoryPage(items: groupHistory[id] ?? [], next_offset: nil))
            } else if components.count == 4, components[1] == "shipments", components[3] == "clear", method == "POST", let id = Int(components[2]), has(id) {
                clearedShipments.insert(id); data = try json(["cleared": true])
            } else { throw unsupported() }
        }
        return (data, HTTPURLResponse(url: url, statusCode: 200, httpVersion: "HTTP/1.1", headerFields: ["Content-Type": "application/json"])!)
    }

    /// A saved practice draft, stored for the session and echoed back as the
    /// daemon's `DraftView` would be. `id` nil mints one; an id is kept.
    private func practiceDraft(_ body: [String: Any], id: Int?) throws -> Data {
        let draftID = id ?? nextDraftID
        if id == nil { nextDraftID += 1 }
        let now = stamp()
        let draft = DraftView(
            id: draftID, reply_to_message_id: body["reply_to_message_id"] as? Int,
            forward_of_message_id: body["forward_of_message_id"] as? Int,
            to: body["to"] as? String ?? "", cc: body["cc"] as? String,
            bcc: body["bcc"] as? String, subject: body["subject"] as? String ?? "",
            body: body["body"] as? String ?? "",
            created_at: id.flatMap { drafts[$0]?.created_at } ?? now, updated_at: now,
            attachments: [])
        drafts[draftID] = draft
        return try encode(draft)
    }

    /// Fictional record facts use the same typed surface as real triage.
    private func recordFacts(_ message: OnboardingRehearsal.Message) -> [AgentRecordProposal] {
        switch message.category {
        case .receipts:
            return [AgentRecordProposal(kind: "receipt", merchant: message.sender,
                amount: [2: 8.50, 8: 4.75, 19: 24.00][message.id], currency: "USD")]
        case .calendar:
            let titles = [10: "Dinner at Juniper Table", 17: "Design review with Maya · Zip", 18: "Nora Vale · The Atlas of Small Things"]
            return [AgentRecordProposal(kind: "event", title: titles[message.id],
                start: AgentSupportedTime(value: eventDate(message.id).ISO8601Format()))]
        case .banking:
            return [AgentRecordProposal(kind: "financial_update", institution: message.sender,
                description: message.explanation)]
        default: return []
        }
    }

    private func update(_ message: OnboardingRehearsal.Message) -> AttentionUpdate {
        let rule = ruleFor(message)
        let tier = tierOverrides[message.id] ?? (rule?.disposition == .surface ? Tier.signal : rule?.disposition == .squelch ? .noise : message.lane == .attention ? .signal : .noise)
        let status = statusOverrides[message.id] ?? (message.isDone || message.lane == .records ? AttentionStatus.done : message.isRead ? .open : .new)
        let importance = importanceOverrides[message.id] ?? (tier == .deadline ? 85 : tier == .signal ? (message.id == 1 ? 80 : 65) : 15)
        return AttentionUpdate(id: message.id, thread_id: threadID(message.id), tier: tier, importance: importance, sender: address(message), one_line: message.subject, reason: message.lane == .reading ? "Newsletter: \(message.explanation)" : message.explanation, deadline: tier == .deadline ? stamp(86400) : nil, matched_rule: rule?.id, from_name: message.sender, status: status, surfaced_at: tier == .noise || message.isRead ? stamp() : nil, resolved_at: status == .done ? stamp() : nil, remind_at: reminders[message.id])
    }
    private func allUpdates() -> [AttentionUpdate] {
        mailbox.messages.map(update).sorted { $0.importance == $1.importance ? $0.id > $1.id : $0.importance > $1.importance }
    }
    private func bandRows(_ band: String) -> [AttentionUpdate] {
        var seen: Set<String> = []
        return allUpdates().filter { row in
            guard row.status != .done, row.tier != .noise else { return false }
            switch band {
            case "standing": return row.tier == .deadline || mailbox.messages.first { $0.id == row.id }?.lane == .attention
            case "new": return row.surfaced_at == nil
            case "open": return row.status == .open
            default: return false
            }
        }.filter { seen.insert($0.thread_id).inserted }
    }
    private func setStatus(_ id: Int, _ status: AttentionStatus) {
        for message in mailbox.messages where threadID(message.id) == threadID(id) {
            statusOverrides[message.id] = status
            mailbox.setDone(message.id, done: status == .done)
            if status == .open { mailbox.open(message.id) }
        }
    }
    private func saveRule(_ body: [String: Any], id: Int?) throws -> Data {
        guard let pattern = body["match_pattern"] as? String, !pattern.isEmpty, let want = body["want"] as? String else {
            throw APIError(.badRequest, 400, "A practice rule needs a sender and instruction")
        }
        // Explicit dispositions are faithfully applied. Free-text filters are
        // retained for editor rehearsal, without pretending a local fixture is AI.
        let disposition = (body["disposition"] as? String).flatMap(Disposition.init(rawValue:)) ?? .filtered
        let ruleID = id ?? nextRuleID
        if id == nil { nextRuleID += 1 }
        rules.removeAll { $0.id == ruleID }
        rules.append(SenderRule(id: ruleID, account_id: 1, match_pattern: pattern, want_text: want, disposition: disposition, updated_at: stamp()))
        if disposition == .squelch {
            if let source = body["source_message_id"] as? Int, has(source) { setStatus(source, .done) }
            if body["sweep"] as? Bool == true {
                for message in mailbox.messages where matches(pattern, address(message)) { setStatus(message.id, .done) }
            }
        }
        return try encode(CreatedRule(rule_id: ruleID, disposition: disposition))
    }
    /// `members` REPLACES the membership, as the daemon's PUT does. An edit
    /// keeps the row's id and what it has sent; a create is a new row with no
    /// history, which is exactly what an undone delete comes back as.
    private func saveGroup(_ body: [String: Any], id: Int?) throws -> Data {
        guard let name = (body["name"] as? String)?.trimmingCharacters(in: .whitespaces), !name.isEmpty else {
            throw APIError(.badRequest, 400, "A practice group needs a name")
        }
        let members = (body["members"] as? [[String: Any]] ?? []).compactMap { member in
            (member["addr"] as? String).map { GroupMember(addr: $0, display_name: member["display_name"] as? String) }
        }
        let existing = id.flatMap { id in groups.first { $0.id == id } }
        let groupID = id ?? nextGroupID
        if id == nil { nextGroupID += 1 }
        let group = SendGroup(
            id: groupID, name: name, slug: name.lowercased().replacingOccurrences(of: " ", with: "-"),
            mode: (body["mode"] as? String).flatMap(GroupMode.init(rawValue:)) ?? .to,
            note: body["note"] as? String ?? "", member_count: members.count, members: members,
            last_sent_at: existing?.last_sent_at, created_at: existing?.created_at ?? stamp(), updated_at: stamp())
        groups.removeAll { $0.id == groupID }
        groups.append(group)
        return try encode(group)
    }
    private func ruleFor(_ message: OnboardingRehearsal.Message) -> SenderRule? {
        rules.filter { matches($0.match_pattern, address(message)) }.sorted {
            let a = $0.match_pattern.filter { $0 == "*" }.count
            let b = $1.match_pattern.filter { $0 == "*" }.count
            return a == b ? $0.id > $1.id : a < b
        }.first
    }
    private func matches(_ pattern: String, _ address: String) -> Bool {
        let expression = "^" + NSRegularExpression.escapedPattern(for: pattern.lowercased()).replacingOccurrences(of: "\\*", with: ".*") + "$"
        return address.lowercased().range(of: expression, options: .regularExpression) != nil
    }
    private func has(_ id: Int) -> Bool { mailbox.messages.contains { $0.id == id } }
    private func threadID(_ id: Int) -> String { "practice-\(id)" }
    private func address(_ message: OnboardingRehearsal.Message) -> String {
        switch message.id {
        case 1: "maya@studio.example"
        case 2, 8: "receipts@cornercoffee.example"
        case 3: "newsletter@catsweekly.example"
        case 4: "alex@friends.example"
        case 5: "tracking@exfed.example"
        case 6: "program@haightssion.example"
        case 9: "statements@harbor.example"
        case 10: "reservations@junipertable.example"
        case 11: "updates@brightly.example"
        case 12: "edition@federaloverstatement.example"
        case 13: "orders@rainforest.example"
        case 14: "orders@rainforest.example"
        case 15: "payments@cedarcredit.example"
        case 16: "transfers@harborsavings.example"
        case 17: "no-reply@zip.example"
        case 18: "events@citylibrary.example"
        default: "receipts@maplebooks.example"
        }
    }
    /// Inline photos are bundled with the app; cache their data URLs once per
    /// process so reopening a newsletter does no file I/O or base64 work.
    private func newsletterPhoto(_ name: String, alt: String) -> String {
        if newsletterPhotos[name] == nil {
            // The bundle (or the directory a test handed over), or nothing: a
            // newsletter without its photograph is still a newsletter.
            let url = Bundle.main.url(forResource: name, withExtension: "jpg")
                ?? Bundle.main.url(forResource: name, withExtension: "jpg", subdirectory: "Resources")
                ?? resourceDirectory?.appendingPathComponent("\(name).jpg")
            if let url, let bytes = try? Data(contentsOf: url) {
                newsletterPhotos[name] = "data:image/jpeg;base64," + bytes.base64EncodedString()
            }
        }
        guard let dataURL = newsletterPhotos[name] else { return "" }
        return "<img src=\"\(dataURL)\" alt=\"\(alt)\" width=\"600\" height=\"400\" style=\"display:block;width:100%;height:auto;aspect-ratio:3/2;object-fit:cover;border:0\">"
    }

    private func newsletterHTML(_ message: OnboardingRehearsal.Message) -> String {
        let content: String
        switch message.id {
        case 3:
            content = """
            <body style="margin:0;background:#f5efdf;padding:24px 12px;color:#293b30;font-family:Georgia,serif">
            <table role="presentation" style="max-width:600px;width:100%;margin:auto;border-collapse:collapse;background:#fffcf3">
            <tr><td style="padding:28px 30px 18px;border-bottom:2px solid #293b30;text-align:center"><div style="font:11px Arial,sans-serif;letter-spacing:3px">THE SUNDAY EDITION · VOL. 028</div><div style="font-size:52px;letter-spacing:-2px;margin:12px 0">Cats Weekly</div><div style="font-size:15px;font-style:italic">Good cats. Questionable decisions.</div></td></tr>
            <tr><td>\(newsletterPhoto("rehearsal-cats", alt: "A cat enjoying a quiet sunlit afternoon"))</td></tr>
            <tr><td style="padding:30px"><div style="font:11px Arial,sans-serif;letter-spacing:2px;color:#956751">THE COVER STORY</div><h1 style="font-size:38px;line-height:1.08;margin:14px 0 18px">The box was the gift.<br>The cat has spoken.</h1><p style="font-size:17px;line-height:1.65">You bought the handwoven bed. You researched the orthopedic cushion. You read thirty-seven reviews. Miso has chosen the cardboard packaging, and would appreciate it if you respected the process.</p><hr style="border:0;border-top:1px solid #ded8c7;margin:28px 0"><h2 style="font-size:23px">Field notes: the sunbeam shift</h2><p style="font-size:16px;line-height:1.7">At 10:14, the warm square moves off the rug. At 10:15, so does the cat. We spent a week tracking this demanding schedule so you don’t have to.</p><div style="padding:22px;background:#e9eedb;margin:26px 0"><h2 style="font-size:23px;margin-top:0">Ask a cat</h2><p style="line-height:1.65"><b>Q:</b> Why knock a full glass off the table?<br><b>A:</b> It was up there. Now it’s down here. Please keep up.</p></div><h2 style="font-size:23px">This week’s small joy</h2><p style="font-size:16px;line-height:1.7">A slow blink from across the room. Send one back.</p></td></tr>
            <tr><td style="padding:22px 30px;border-top:1px solid #ded8c7;font:11px/1.6 Arial,sans-serif;color:#6a7567">You’re receiving Cats Weekly because the internet needed one more cat newsletter.<br>Fictional practice edition · All the cats, none of the urgency.</td></tr></table></body>
            """
        case 6:
            content = """
            <body style="margin:0;background:#101010;padding:24px 12px;color:#eeeeea;font-family:Arial,Helvetica,sans-serif">
            <table role="presentation" style="max-width:600px;width:100%;margin:auto;border-collapse:collapse;background:#171717">
            <tr><td style="padding:24px;border-top:8px solid #d9ff43"><div style="font:11px monospace;letter-spacing:2px;color:#d9ff43">PROGRAM 041 / AFTER DARK</div><div style="font-size:49px;line-height:1;letter-spacing:-3px;font-weight:900;margin-top:22px">HAIGHTSSION</div></td></tr>
            <tr><td>\(newsletterPhoto("rehearsal-club", alt: "Monochrome industrial club interior with dramatic light"))</td></tr>
            <tr><td style="padding:26px 24px"><h1 style="font-size:46px;font-weight:900;line-height:.98;letter-spacing:-2px;margin:0 0 24px">NO PHONES.<br>LONG NIGHT.</h1><p style="font-size:15px;line-height:1.7;color:#c9c9c5">Concrete. Low light. A room built around the sound. Our next all-night session moves from stripped percussion into the slow, heavy end of morning.</p><div style="border-top:1px solid #777;border-bottom:1px solid #777;padding:18px 0;margin:26px 0;font:12px/1.8 monospace;letter-spacing:1px">SATURDAY / DOORS 23:00 / UNTIL LATE</div><div style="color:#d9ff43;font:11px monospace;letter-spacing:2px">MAIN FLOOR</div><p style="font:18px/2 monospace">23:00 — NERA<br>02:00 — ECHO NULL [LIVE]<br>05:00 — SABLE FORM</p><div style="color:#d9ff43;font:11px monospace;letter-spacing:2px;margin-top:30px">ROOM TWO</div><p style="font-size:15px;line-height:1.7;color:#c9c9c5">An intimate listening room. Dub, broken rhythms, and somewhere to catch your breath.</p><h2 style="font-size:24px;letter-spacing:-.5px;margin-top:34px">COME AS YOU ARE.</h2><p style="font-size:14px;line-height:1.8;color:#c9c9c5">Respect the room. Ask before touching. Leave your camera in your pocket. If something feels wrong, speak to the door team or anyone wearing a green band.</p></td></tr>
            <tr><td style="padding:20px 24px;border-top:1px solid #555;color:#999;font:10px/1.8 monospace">DOOR SALES ONLY / SUBJECT TO CAPACITY<br>FICTIONAL EVENT ANNOUNCEMENT · PRACTICE INBOX</td></tr></table></body>
            """
        default:
            content = """
            <body style="margin:0;background:#e8e5dc;padding:24px 12px;color:#181818;font-family:Georgia,'Times New Roman',serif">
            <table role="presentation" style="max-width:600px;width:100%;margin:auto;border-collapse:collapse;background:#fffdf6">
            <tr><td style="padding:22px 26px 16px;border-top:5px solid #181818;text-align:center"><div style="font:10px Arial,sans-serif;letter-spacing:2px">SATIRE / FICTION / AN ENTIRELY INVENTED EDITION</div><div style="font-size:38px;font-weight:bold;line-height:1.05;margin:14px 0">The Federal<br>Overstatement</div><div style="font-size:12px;font-style:italic;border-top:1px solid #181818;border-bottom:3px double #181818;padding:10px 0">All the news that confidently exceeds the available evidence.</div></td></tr>
            <tr><td>\(newsletterPhoto("rehearsal-politics", alt: "An empty government-style chamber overwhelmed by paperwork"))</td></tr>
            <tr><td style="padding:26px"><div style="font:10px Arial,sans-serif;letter-spacing:2px">NATIONAL AFFAIRS</div><h1 style="font-size:34px;line-height:1.08;margin:12px 0 18px">Congress forms bipartisan committee to locate previous committee</h1><p style="font-size:16px;line-height:1.65"><b>WASHINGTON</b> — In a rare display of unity, lawmakers approved a new commission to determine which room the old commission was meeting in. Its first act was to request a larger room.</p><hr style="border:0;border-top:1px solid #aaa;margin:26px 0"><h2 style="font-size:24px;line-height:1.15">White House unveils strategic reserve of strongly worded statements</h2><p style="font-size:15px;line-height:1.65">Officials assured the public that supplies remain adequate for six consecutive news cycles, provided no one uses “deeply” more than twice per paragraph.</p><h2 style="font-size:24px;line-height:1.15;margin-top:28px">Nation’s infrastructure now 40% ceremonial scissors</h2><p style="font-size:15px;line-height:1.65">A new report praised the country’s readiness to open things while raising questions about whether the things themselves had been built.</p><div style="border:1px solid #181818;padding:20px;margin-top:28px"><div style="font:10px Arial,sans-serif;letter-spacing:2px">THE EDITORIAL BOARD</div><h2 style="font-size:24px;line-height:1.15">At last, a five-year plan for next week</h2><p style="font-size:15px;line-height:1.65">The plan’s authors described its first milestone as “scheduling the meeting about the milestones.”</p></div></td></tr>
            <tr><td style="padding:20px 26px;border-top:3px double #181818;font:11px/1.6 Arial,sans-serif">SATIRE. Every story is fictional. No actual officials or current events are depicted.<br>The Federal Overstatement · Practice edition.</td></tr></table></body>
            """
        }
        return "<!doctype html><html>" + content + "</html>"
    }

    private func brightlyHTML() -> String {
        let date = DateFormatter()
        date.dateStyle = .long
        let effectiveDate = date.string(from: referenceDate.addingTimeInterval(30 * 86400))
        return """
        <!doctype html><html><body style="margin:0;background:#f3f1e9;padding:32px 16px;color:#161813;font-family:Arial,Helvetica,sans-serif">
        <table role="presentation" style="width:100%;max-width:600px;margin:auto;border-collapse:collapse;background:#ffffff">
        <tr><td style="padding:46px 28px 38px;text-align:center;border-bottom:1px solid #e8e7df">
        <span aria-label="Brightly sunburst" style="display:inline-block;vertical-align:middle;width:72px;height:72px;margin-right:12px;background:#ffe500;clip-path:polygon(50% 0%,58% 20%,75% 7%,73% 29%,93% 25%,80% 43%,100% 50%,80% 58%,93% 75%,71% 73%,75% 93%,57% 80%,50% 100%,42% 80%,25% 93%,27% 71%,7% 75%,20% 57%,0% 50%,20% 42%,7% 25%,29% 27%,25% 7%,43% 20%)"></span><span style="display:inline-block;vertical-align:middle;font-size:62px;font-weight:900;letter-spacing:-4px;line-height:1;color:#11120e">brightly</span>
        </td></tr>
        <tr><td style="padding:36px 40px 16px"><p style="margin:0 0 12px;font-size:11px;font-weight:700;letter-spacing:1.8px;color:#73766b">A NOTE ABOUT YOUR ACCOUNT</p><h1 style="margin:0 0 18px;font-size:32px;line-height:1.15;letter-spacing:-1px">We’re updating our<br>Terms of Service.</h1><p style="margin:0;padding:12px 16px;background:#f7f6ec;font-size:13px;line-height:1.5">Effective <strong>\(effectiveDate)</strong></p></td></tr>
        <tr><td style="padding:10px 40px 32px;font-size:15px;line-height:1.7">
        <p style="margin:0 0 18px">Hi Jamie,</p><p style="margin:0 0 26px">We’re updating our Terms of Service to make it clearer how your Brightly account and subscription work. The updated terms take effect on <strong>\(effectiveDate)</strong>.</p>
        <h2 style="margin:0 0 18px;font-size:20px;line-height:1.3">Here’s what’s changing</h2>
        <p style="margin:0 0 18px"><strong>Subscription renewals</strong><br>We’ve clarified when a subscription renews, how billing dates are calculated, and where to manage your plan.</p>
        <p style="margin:0 0 18px"><strong>Account controls</strong><br>We’ve added more detail about exporting your content, closing an account, and what happens to your data afterward.</p>
        <p style="margin:0 0 26px"><strong>Service updates</strong><br>We’ve explained how we communicate changes to Brightly and where to find support if something goes wrong.</p>
        <h2 style="margin:0 0 14px;font-size:20px;line-height:1.3">What you need to do</h2>
        <p style="margin:0 0 18px"><strong>Nothing.</strong> These updates do not change your current price or plan. By continuing to use Brightly after the effective date, you agree to the updated terms.</p>
        <p style="margin:0 0 26px">You can review the full Terms of Service in <strong>Settings → Legal</strong> at any time.</p>
        <p style="margin:0">Thanks for making room for Brightly.<br><strong>The Brightly team</strong></p>
        </td></tr>
        <tr><td style="padding:26px 40px;background:#efeee5;font-size:11px;line-height:1.65;color:#75766d"><p style="margin:0 0 12px">You’re receiving this required service notice because you have a Brightly account. Legal and account notices are sent even if you’ve opted out of marketing emails, and you cannot unsubscribe from these notices. Please do not reply to this automated message.</p><p style="margin:0 0 12px">Brightly, Inc. · 240 Sunbeam Avenue<br>San Francisco, CA 94103</p><p style="margin:0">Fictional company and terms, created for the Passband guide.</p></td></tr>
        </table></body></html>
        """
    }

    private func rainforestHTML(_ message: OnboardingRehearsal.Message) -> String {
        let outForDelivery = message.id == 13
        let date = DateFormatter()
        date.dateStyle = .full
        let arrival = outForDelivery ? "Today" : date.string(from: referenceDate.addingTimeInterval(172800))
        let headline = outForDelivery ? "Your package is almost there." : "Your order is on its way."
        let status = outForDelivery ? "Out for delivery" : "Shipped"
        let order = outForDelivery ? "RF-38109" : "RF-72841"
        let item = outForDelivery ? "Linen notebook set" : "Windowsill herb planter"
        let detail = outForDelivery ? "3 notebooks · Natural linen" : "Sage · Planter, drainage tray &amp; care guide"
        let note = outForDelivery
            ? "Your order is on the delivery vehicle and should arrive today. No signature is required. If you’re not home, the carrier may leave your package in a safe place."
            : "We’ve packed your order and handed it to the carrier. Tracking updates may take a little time to appear."
        let closing = outForDelivery
            ? "We’ll send you another update when your package has been delivered."
            : "Please check the contents when your package arrives and keep the packaging until you’re happy with your order."
        let deliveryColor = outForDelivery ? "#007d69" : "#68747c"
        return """
        <!doctype html><html><body style="margin:0;background:#eaeded;padding:24px 12px;color:#17232d;font-family:Arial,Helvetica,sans-serif">
        <table role="presentation" style="width:100%;max-width:600px;margin:auto;border-collapse:collapse;background:#ffffff">
        <tr><td style="padding:24px 32px 28px;background:#232F3E;color:#ffffff">
        <div aria-label="rainforest" style="display:inline-block;position:relative;padding-bottom:13px;font-size:36px;line-height:1;font-weight:700;letter-spacing:-1.7px;color:#ffffff">rainforest<span aria-hidden="true" style="position:absolute;left:35px;bottom:0;width:108px;height:17px;border-bottom:4px solid #FF9900;border-radius:0 0 60% 60%;transform:rotate(-5deg)"></span><span aria-hidden="true" style="position:absolute;left:137px;bottom:6px;width:0;height:0;border-left:8px solid #FF9900;border-top:4px solid transparent;border-bottom:4px solid transparent;transform:rotate(-30deg)"></span></div>
        </td></tr>
        <tr><td style="padding:30px 32px 22px"><p style="margin:0 0 16px;font-size:14px;color:#52616d">Hello Jamie,</p><h1 style="margin:0 0 14px;font-size:29px;line-height:1.2;letter-spacing:-.6px;font-weight:400">\(headline)</h1><p style="margin:0;font-size:14px;line-height:1.7;color:#46545e">\(note)</p></td></tr>
        <tr><td style="padding:0 32px 28px"><div style="padding:22px 24px;background:#f1f8f6;border-left:4px solid #007d69"><div style="font-size:12px;color:#52616d;margin-bottom:7px">EXPECTED DELIVERY</div><div style="font-size:24px;line-height:1.3;font-weight:700;color:#007d69">\(arrival)</div><div style="margin-top:10px;font-size:13px;color:#46545e">Jamie · Portland, OR 97205</div></div></td></tr>
        <tr><td style="padding:0 32px 28px"><table role="presentation" style="width:100%;border-collapse:collapse;text-align:center;font-size:11px"><tr><td style="width:33%;border-top:4px solid #007d69;padding:11px 0;color:#007d69;font-weight:700">Shipped</td><td style="width:34%;border-top:4px solid \(outForDelivery ? "#007d69" : "#d9dfe2");padding:11px 0;color:\(deliveryColor);font-weight:700">Out for delivery</td><td style="width:33%;border-top:4px solid #d9dfe2;padding:11px 0;color:#68747c">Delivered</td></tr></table></td></tr>
        <tr><td style="padding:0 32px 28px"><div style="border-top:1px solid #d9dfe2;padding-top:24px"><h2 style="font-size:18px;font-weight:400;margin:0 0 6px">In this shipment</h2><p style="font-size:12px;color:#68747c;margin:0 0 20px">Order #\(order)</p><table role="presentation" style="width:100%;border-collapse:collapse"><tr><td style="width:76px;vertical-align:top"><div aria-label="Parcel" style="width:52px;height:48px;background:#d8b88d;border:1px solid #bb9465;position:relative"><div style="width:11px;height:48px;background:#eee0c6;margin:auto"></div></div></td><td style="vertical-align:top"><div style="font-size:16px;line-height:1.4;color:#08788a">\(item)</div><div style="font-size:12px;color:#68747c;margin-top:6px;line-height:1.5">\(detail)<br>Quantity: 1 · Sold by Rainforest</div></td></tr></table></div></td></tr>
        <tr><td style="padding:0 32px 28px"><table role="presentation" style="width:100%;border-collapse:collapse;background:#f5f7f7;font-size:13px;line-height:1.6"><tr><td style="padding:16px 18px;color:#68747c">Shipment status<br>Tracking number</td><td style="padding:16px 18px;text-align:right">\(status)<br><span style="font-family:monospace">PRACTICE-\(message.id)</span></td></tr></table><p style="font-size:13px;line-height:1.7;color:#52616d;margin:20px 0 0">\(closing)</p></td></tr>
        <tr><td style="padding:24px 32px;background:#f7f8f8;border-top:1px solid #d9dfe2;font-size:11px;line-height:1.7;color:#68747c"><p style="margin:0 0 12px">Questions about your delivery? Visit Customer Support in your Rainforest account and reference order #\(order).</p><p style="margin:0 0 12px">This is an automated shipment notification. Please do not reply to this email.</p><p style="margin:0">© Rainforest Retail · Fictional delivery for the Passband guide.</p></td></tr>
        </table></body></html>
        """
    }

    private func calendarExperienceHTML(_ message: OnboardingRehearsal.Message) -> String {
        let date = DateFormatter()
        date.dateStyle = .full
        date.timeStyle = .short
        let when = date.string(from: eventDate(message.id)) + " " + (TimeZone.current.abbreviation(for: eventDate(message.id)) ?? TimeZone.current.identifier)
        if message.id == 10 {
            return """
            <!doctype html><html><body style="margin:0;padding:28px 14px;background:#ebe9df;color:#243c30;font-family:Georgia,serif">
            <table role="presentation" style="width:100%;max-width:600px;margin:auto;border-collapse:collapse;background:#faf8ef">
            <tr><td style="padding:32px;text-align:center;background:#243c30;color:#f7eed9"><div style="font-size:32px;line-height:1">✳</div><div style="font-size:30px;letter-spacing:4px;margin-top:14px">JUNIPER TABLE</div><div style="font:10px Arial,sans-serif;letter-spacing:3px;margin-top:12px">SEASONAL KITCHEN · PORTLAND</div></td></tr>
            <tr><td style="padding:40px 38px 22px;text-align:center"><p style="font:11px Arial,sans-serif;letter-spacing:2px;color:#75806e;margin:0 0 18px">YOUR EVENING IS RESERVED</p><h1 style="font-size:39px;line-height:1.15;font-weight:normal;margin:0 0 20px">A table for three.<br>A little time together.</h1><p style="font-size:16px;line-height:1.7;margin:0">Jamie, we’re looking forward to having you.<br>Come hungry. Stay a while.</p></td></tr>
            <tr><td style="padding:12px 38px 30px"><table role="presentation" style="width:100%;border-collapse:collapse;border-top:1px solid #c8cdbb;border-bottom:1px solid #c8cdbb"><tr><td style="padding:20px 0;font:11px Arial,sans-serif;color:#75806e;vertical-align:top">WHEN</td><td style="padding:20px 0 20px 18px;font-size:17px;text-align:right">\(when)</td></tr><tr><td style="padding:0 0 18px;font:11px Arial,sans-serif;color:#75806e">YOUR TABLE</td><td style="padding:0 0 18px;text-align:right;font-size:16px">3 guests · Main dining room</td></tr><tr><td style="padding:0 0 20px;font:11px Arial,sans-serif;color:#75806e">RESERVATION</td><td style="padding:0 0 20px;text-align:right;font:13px monospace">JT-60218</td></tr></table></td></tr>
            <tr><td style="padding:0 38px 32px"><div style="padding:26px;background:#efeddf"><p style="margin:0 0 16px;font:11px Arial,sans-serif;letter-spacing:2px;color:#75806e">A TASTE OF THE SEASON</p><div style="font-size:21px;line-height:1.6">Warm sourdough &amp; cultured butter<br>Roasted squash, sage &amp; hazelnuts<br>Pear tart with vanilla cream</div><p style="font-size:12px;line-height:1.6;color:#75806e;margin:16px 0 0">A few things we’re cooking this week. Our menu follows the market; dishes may change.</p></div></td></tr>
            <tr><td style="padding:0 38px 34px;font:13px/1.8 Arial,sans-serif"><h2 style="font:24px Georgia,serif;margin:0 0 12px">Before you arrive</h2><p style="margin:0 0 14px">Find us at <strong>214 Alder Street, Portland</strong>. The entrance is just beside the green awning. We’ll hold your table for 15 minutes.</p><p style="margin:0">Allergies, a special occasion, or a change of plans? Contact the restaurant with your reservation number so we can take care of the details.</p></td></tr>
            <tr><td style="padding:22px 38px;border-top:1px solid #d8dacb;text-align:center;font:10px/1.7 Arial,sans-serif;color:#75806e">JUNIPER TABLE · GOOD FOOD, GOOD COMPANY<br>Fictional restaurant and reservation for the Passband guide.</td></tr>
            </table></body></html>
            """
        }
        return """
        <!doctype html><html><body style="margin:0;padding:28px 14px;background:#e9e7f0;color:#24223b;font-family:Arial,Helvetica,sans-serif">
        <table role="presentation" style="width:100%;max-width:600px;margin:auto;border-collapse:collapse;background:#ffffff">
        <tr><td style="padding:22px 32px;background:#24223b;color:#fff"><span style="font-family:Georgia,serif;font-size:25px">City Library</span><span style="float:right;margin-top:8px;font-size:10px;letter-spacing:2px;color:#cac6e0">AFTER HOURS</span></td></tr>
        <tr><td style="padding:36px 32px;background:#eee8fb"><p style="font-size:11px;letter-spacing:2px;margin:0 0 18px;color:#74608f">AUTHOR TALK · READING · CONVERSATION</p><table role="presentation" style="width:100%;border-collapse:collapse"><tr><td style="vertical-align:top;padding-right:22px"><h1 style="font:38px/1.1 Georgia,serif;margin:0 0 20px">An evening<br>with Nora Vale.</h1><p style="font-size:14px;line-height:1.7;margin:0">On overlooked places, unexpected connections, and the stories hiding in everyday life.</p></td><td style="width:145px;vertical-align:top"><div style="background:#f5b959;border-left:7px solid #ce8c30;box-shadow:6px 8px 0 #d4c9e5;padding:20px 12px;height:182px"><div style="font:11px Georgia,serif;letter-spacing:2px">NORA VALE</div><div style="height:1px;background:#24223b;margin:17px 0"></div><div style="font:25px/1.04 Georgia,serif">The Atlas<br>of Small<br>Things</div><div style="font-size:28px;margin-top:12px">✦</div></div></td></tr></table></td></tr>
        <tr><td style="padding:30px 32px 18px"><p style="font-size:11px;letter-spacing:2px;color:#74608f;margin:0 0 12px">YOU’RE ON THE LIST</p><h2 style="font:29px Georgia,serif;margin:0 0 14px">Your seat is saved, Jamie.</h2><p style="font-size:14px;line-height:1.8;margin:0">Join novelist Nora Vale for a reading from <em>The Atlas of Small Things</em>, followed by a conversation with our community librarian and questions from the audience.</p></td></tr>
        <tr><td style="padding:10px 32px 28px"><div style="border:1px solid #d8d3e5;border-left:5px solid #74608f;padding:22px"><div style="font-size:11px;color:#74608f;letter-spacing:1px;margin-bottom:9px">ADMIT ONE · FREE COMMUNITY EVENT</div><strong style="font-size:16px;line-height:1.6">\(when)</strong><p style="font-size:14px;line-height:1.6;margin:10px 0">City Library · Main Hall<br>Registration: <span style="font-family:monospace">CL-49371</span></p><div style="border-top:1px dashed #c5bdd5;padding-top:12px;font-size:12px;color:#74608f">Show this email at the door. No printing needed.</div></div></td></tr>
        <tr><td style="padding:0 32px 30px"><h2 style="font:23px Georgia,serif;margin:0 0 18px">The evening’s program</h2><table role="presentation" style="width:100%;font-size:13px;line-height:1.7;border-collapse:collapse"><tr><td style="padding:8px 14px 8px 0;color:#74608f;vertical-align:top">BEFORE</td><td style="padding:8px 0">Doors open 30 minutes early. Browse the book display and find your seat.</td></tr><tr><td style="padding:8px 14px 8px 0;color:#74608f;vertical-align:top">DURING</td><td style="padding:8px 0">A reading, a conversation, and your questions.</td></tr><tr><td style="padding:8px 14px 8px 0;color:#74608f;vertical-align:top">AFTER</td><td style="padding:8px 0">Meet the author and stay for the book signing.</td></tr></table><p style="font-size:12px;line-height:1.8;color:#74608f;margin:20px 0 0">Seating is unassigned. The Main Hall has step-free access. If your plans change or you need an accommodation, contact our events team with your registration number.</p></td></tr>
        <tr><td style="padding:22px 32px;background:#24223b;font-size:11px;line-height:1.8;color:#d6d0e8">CITY LIBRARY · A PLACE FOR EVERY STORY<br>Fictional author, book, and event for the Passband guide.</td></tr>
        </table></body></html>
        """
    }

    private func eventDate(_ id: Int) -> Date {
        let calendar = Calendar.current
        let tomorrow = calendar.date(byAdding: .day, value: 1, to: calendar.startOfDay(for: referenceDate))!
        let hour = id == 17 ? 10 : id == 18 ? 19 : 18
        return calendar.date(byAdding: .hour, value: hour, to: tomorrow)!
    }
    private func renderedBody(_ message: OnboardingRehearsal.Message) -> String {
        let date = DateFormatter()
        date.dateStyle = .full
        date.timeStyle = .short
        let event = date.string(from: eventDate(message.id)) + " " + (TimeZone.current.abbreviation(for: eventDate(message.id)) ?? TimeZone.current.identifier)
        date.timeStyle = .none
        return message.body
            .replacingOccurrences(of: "{{event_time}}", with: event)
            .replacingOccurrences(of: "{{delivery_date}}", with: date.string(from: referenceDate.addingTimeInterval(message.id == 14 ? 172800 : 86400)))
            .replacingOccurrences(of: "{{payment_date}}", with: date.string(from: referenceDate.addingTimeInterval(86400)))
            .replacingOccurrences(of: "{{terms_date}}", with: date.string(from: referenceDate.addingTimeInterval(30 * 86400)))
    }

    /// Provider-style email markup goes through the same production HTML reader
    /// as received mail. All styling is inline; there are no remote resources,
    /// links, tracking pixels, or scripts. The plain-text alternative stays intact.
    private func renderedHTML(_ message: OnboardingRehearsal.Message) -> String? {
        if message.category == .reading { return newsletterHTML(message) }
        if message.id == 11 { return brightlyHTML() }
        if [10, 18].contains(message.id) { return calendarExperienceHTML(message) }
        if [13, 14].contains(message.id) { return rainforestHTML(message) }
        guard message.lane == .records else { return nil }
        func escape(_ value: String) -> String {
            value.replacingOccurrences(of: "&", with: "&amp;")
                .replacingOccurrences(of: "<", with: "&lt;")
                .replacingOccurrences(of: ">", with: "&gt;")
                .replacingOccurrences(of: "\"", with: "&quot;")
        }
        let accent = message.id == 17 ? "#0b5cff" : message.id == 5 ? "#51258a" : [13, 14].contains(message.id) ? "#c47708" : message.category == .banking ? "#155b53" : "#283d4b"
        let type = message.category == .receipts ? "monospace" : "Arial, sans-serif"
        let providerHeader = message.id == 5
            ? "<span aria-label='ExFed' style='font-family:Arial,Helvetica,sans-serif;font-size:42px;font-weight:900;letter-spacing:-2px'><span style='color:#51258a'>Ex</span><span style='color:#f46a20'>Fed</span></span><div style='font-size:10px;letter-spacing:3px;color:#68737d;margin-top:8px'>DELIVERY SERVICES</div>"
            : escape(message.sender)
        return """
        <!doctype html><html><body style="margin:0;background:#f4f5f7;padding:28px 18px;color:#25313c;font-family:Arial,sans-serif">
        <table role="presentation" style="width:100%;max-width:560px;margin:auto;border-collapse:collapse;background:#ffffff;border:1px solid #e3e6e9">
        <tr><td style="padding:24px 30px;border-bottom:3px solid \(accent);font-size:24px;font-weight:700;color:\(accent)">\(providerHeader)</td></tr>
        <tr><td style="padding:28px 30px"><h1 style="margin:0 0 24px;font-size:22px;line-height:1.3">\(escape(message.subject))</h1><div style="white-space:pre-wrap;overflow-wrap:anywhere;font-family:\(type);font-size:14px;line-height:1.65">\(escape(renderedBody(message)))</div></td></tr>
        <tr><td style="padding:18px 30px;background:#f8f9fa;border-top:1px solid #e3e6e9;font-size:11px;line-height:1.5;color:#68737d">Fictional email for your Passband practice inbox. No payment, delivery, or meeting is associated with this message.</td></tr>
        </table></body></html>
        """
    }
    private func stamp(_ offset: TimeInterval = 0) -> String { referenceDate.addingTimeInterval(offset).ISO8601Format() }
    /// A short list rather than the detector's ~68: enough to try the card on.
    /// Sorted by name, the way the daemon sends it.
    static let practiceLanguages: [TranslateLanguage] = [
        ("zh", "Chinese"), ("nl", "Dutch"), ("fr", "French"), ("de", "German"),
        ("hi", "Hindi"), ("it", "Italian"), ("ja", "Japanese"), ("ko", "Korean"),
        ("pt", "Portuguese"), ("es", "Spanish"),
    ].map { TranslateLanguage(code: $0.0, name: $0.1) }

    /// Active, because the practice inbox shows translated mail.
    private func translateConfig() throws -> Data {
        try encode(TranslateConfig(active: true, read_languages: readLanguages, available: Self.practiceLanguages))
    }

    private func encode<T: Encodable>(_ value: T) throws -> Data { try JSONEncoder().encode(value) }
    private func json(_ value: [String: Any]) throws -> Data { try JSONSerialization.data(withJSONObject: value, options: [.sortedKeys]) }
    private func unsupported() -> APIError { APIError(.badRequest, 400, "This action isn't available in the practice inbox. Nothing was sent or changed outside this rehearsal.") }
    private func missing() -> APIError { APIError(.notFound, 404, "This message is not in the practice inbox.") }
}

#if DEBUG
    /// MAIL IN FRENCH, for seeing the reader's translation strip without a
    /// daemon that translates (PassbandiOS FixtureStage's `translated` scenes,
    /// and any Mac hook that wants it). Debug only, and never part of the
    /// lesson: the practice inbox is English, and a stranger's first look at
    /// Passband is not the place to meet a language they may not read.
    extension RehearsalAPI {
        enum TranslationFixture {
            enum Scene: String, CaseIterable, Sendable {
                /// One message, translated, in HTML.
                case done
                /// One plain message whose fact check failed.
                case incomplete
                /// One plain message still being translated, which stays pending.
                case pending
                /// ARCHIVE MAIL: one plain message whose translation is
                /// deferred until the reader asks. It stays deferred under any
                /// number of reads; the reader's ask makes it pending, and the
                /// next refetch after that lands it.
                case deferred
                /// All three in one thread, oldest first, where the pending one
                /// lands as done on the third fetch (the reader's third refetch).
                case all
            }

            static let doneHTML = """
                <div style="font-family:Georgia,serif;max-width:560px;margin:0 auto;color:#2b2b2b">
                <h2 style="font-weight:normal;margin:0 0 12px">Votre réservation est confirmée</h2>
                <p>Bonjour Jamie,</p>
                <p>Nous avons le plaisir de confirmer votre table pour <strong>3 personnes</strong>
                le <strong>jeudi 24 octobre à 20h00</strong>.</p>
                <p>Numéro de confirmation : <strong>LB-48213</strong><br>
                Adresse : 12 rue des Martyrs, 75009 Paris</p>
                <p>Merci de nous prévenir au moins 24 heures à l'avance en cas d'annulation.
                <a href="https://example.com/reservation">Gérer ma réservation</a></p>
                <p>À très bientôt,<br>L'équipe du Bistrot Lumière</p>
                </div>
                """

            static let doneHTMLEnglish = """
                <div style="font-family:Georgia,serif;max-width:560px;margin:0 auto;color:#2b2b2b">
                <h2 style="font-weight:normal;margin:0 0 12px">Your reservation is confirmed</h2>
                <p>Hello Jamie,</p>
                <p>We are pleased to confirm your table for <strong>3 people</strong>
                on <strong>Thursday, October 24 at 8:00 PM</strong>.</p>
                <p>Confirmation number: <strong>LB-48213</strong><br>
                Address: 12 rue des Martyrs, 75009 Paris</p>
                <p>Please let us know at least 24 hours in advance if you need to cancel.
                <a href="https://example.com/reservation">Manage my reservation</a></p>
                <p>See you very soon,<br>The Bistrot Lumière team</p>
                </div>
                """

            static let incompletePlain = """
                Bonjour,

                Votre relevé de compte du mois de septembre est disponible.
                Solde au 30/09 : 1.234,56 €
                Prélèvement prévu le 05/10 : 89,90 € (référence FR-7730-XQ)

                Cordialement,
                Banque Horizon
                """

            static let incompleteEnglish = """
                Hello,

                Your account statement for September is available.
                Balance on 09/30: 1,234.56 €
                Direct debit scheduled for 10/05: 89.90 €

                Kind regards,
                Banque Horizon
                """

            static let pendingPlain = """
                Salut Jamie !

                On se retrouve toujours samedi au marché ? Je pensais arriver vers 10h, et on pourrait déjeuner ensuite chez Paulette.

                Bises,
                Camille
                """

            static let pendingEnglish = """
                Hi Jamie!

                Are we still meeting at the market on Saturday? I was thinking of getting there around 10, and we could have lunch at Paulette's afterwards.

                Love,
                Camille
                """

            private static func translation(
                _ status: ClientTranslation.Status, subject: String, content: String,
                html: String? = nil, missing: [String]? = nil
            ) -> ClientTranslation {
                ClientTranslation(
                    source_lang: "fr", source_language: "French", status: status, subject: subject,
                    content: content, html: html, missing_facts: missing)
            }

            private static func message(
                id: Int, name: String, addr: String, minutesAgo: Double, subject: String,
                content: String, html: String? = nil, translation: ClientTranslation?
            ) -> ClientMessage {
                ClientMessage(
                    id: id, from_addr: addr, from_name: name,
                    received_at: Date().addingTimeInterval(-minutesAgo * 60).ISO8601Format(),
                    content: content, html: html, subject: subject, attachments: [],
                    is_sent: false, is_spam: false, sender_known: true, translation: translation)
            }

            static func done() -> ClientMessage {
                message(
                    id: 9101, name: "Bistrot Lumière", addr: "reservations@bistrot-lumiere.example",
                    minutesAgo: 90, subject: "Votre réservation est confirmée",
                    content: "Votre réservation est confirmée", html: doneHTML,
                    translation: translation(
                        .done, subject: "Your reservation is confirmed",
                        content: "Your reservation is confirmed", html: doneHTMLEnglish))
            }

            static func incomplete() -> ClientMessage {
                message(
                    id: 9102, name: "Banque Horizon", addr: "releves@banque-horizon.example",
                    minutesAgo: 45, subject: "Votre relevé de septembre",
                    content: incompletePlain,
                    translation: translation(
                        .incomplete, subject: "Your September statement", content: incompleteEnglish,
                        missing: ["FR-7730-XQ"]))
            }

            static func pending(landed: Bool = false, waiting: ClientTranslation.Status = .pending)
                -> ClientMessage
            {
                message(
                    id: 9103, name: "Camille Durand", addr: "camille@exemple.example",
                    minutesAgo: 2, subject: "Samedi au marché ?", content: pendingPlain,
                    translation: landed
                        ? translation(.done, subject: "Saturday at the market?", content: pendingEnglish)
                        : translation(waiting, subject: "", content: ""))
            }

            /// The versions to stage for `scene` under `threadId`, first fetch
            /// first. Seed the prefetch cache with the first one too, so the
            /// reader opens on it rather than on the practice thread it replaces.
            static func versions(_ scene: Scene, threadId: String) -> [ClientThreadView] {
                func view(_ subject: String, _ messages: [ClientMessage]) -> ClientThreadView {
                    ClientThreadView(
                        cache_allowed: true, thread_id: threadId, subject: subject, messages: messages)
                }
                switch scene {
                case .done: return [view("Votre réservation est confirmée", [done()])]
                case .incomplete: return [view("Votre relevé de septembre", [incomplete()])]
                case .pending: return [view("Samedi au marché ?", [pending()])]
                case .deferred:
                    let subject = "Samedi au marché ?"
                    return [
                        view(subject, [pending(waiting: .deferred)]),
                        view(subject, [pending()]),
                        view(subject, [pending(landed: true)]),
                    ]
                case .all:
                    let subject = "Votre réservation est confirmée"
                    return [
                        view(subject, [done(), incomplete(), pending()]),
                        view(subject, [done(), incomplete(), pending()]),
                        view(subject, [done(), incomplete(), pending(landed: true)]),
                    ]
                }
            }
        }
    }
#endif
