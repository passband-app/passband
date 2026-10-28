// A WAY TO SEE THE PHONE WITHOUT A DAEMON OR A THUMB. Debug builds only.
//
// The simulator has no tap tool and a fresh one has no account, so a change to
// this shell could be compiled and never looked at. Launched with
// `--fixture-inbox` the app boots straight onto the practice inbox's fixture
// mail (the same `RehearsalAPI` the Mac's lesson uses), and two more arguments
// put a screen up for a screenshot:
//
//   xcrun simctl launch <sim> app.passband.client.ios --fixture-inbox \
//       --fixture-tab mail --fixture-scene thread
//
// Nothing here ships: the file is fenced, and so is its one call site.

#if DEBUG
    import Foundation
    import Observation

    @MainActor
    enum FixtureStage {
        private static let args = ProcessInfo.processInfo.arguments

        static let on = args.contains("--fixture-inbox")

        private static func value(_ flag: String) -> String? {
            guard let i = args.firstIndex(of: flag) else { return nil }
            return args[safe: i + 1]
        }

        /// `sitrep` (the default), `records`, `calendar`, `mail` or `search`.
        static var tab: String? { value("--fixture-tab") }
        /// Turns the hidden `calendar` feature on over a staged week of plans
        /// (FixtureCalendar), so the bar reads Sitrep, Calendar, Mail.
        static let calendar = args.contains("--fixture-calendar")
        /// Calendar scenes, read by MobileCalendarView itself: `calendar-month`
        /// opens the strip into the month, `calendar-detail` pushes the
        /// likely ghost's page.
        static var calendarMonthOpen: Bool { scene == "calendar-month" }
        static var calendarDetail: String? { scene == "calendar-detail" ? "g:2" : nil }
        /// What to raise once the board has loaded; see `stage`.
        static var scene: String? { value("--fixture-scene") }
        /// Where a What's New vignette's clock is stopped, in seconds into its
        /// loop: `--fixture-vignette-t 1.2`. A screenshot catches one frame,
        /// and a scene is judged at several. Absent, the vignette plays.
        static var vignetteTime: Double? { value("--fixture-vignette-t").flatMap(Double.init) }

        /// A press of the reader's checkmark, played by the reader itself once
        /// its thread is up (`ThreadViewer.rehearseBar`): nothing can touch the
        /// simulator, and the button's two verbs live in a view's private state.
        enum BarScript {
            /// Thumb down, and up again after `held`; nil never lets go.
            case press(held: Duration?)
            /// No press at all: the ring, stopped at `pinnedHold`.
            case pinned
        }

        private static var barScript: BarScript?
        /// Where the hold ring's clock is stopped for a screenshot, 0...1.
        private(set) static var pinnedHold: Double?

        /// Handed out ONCE. A tap on a stack mounts the next email's reader,
        /// and a script that survived would tap that one too, and the next.
        static func takeBarScript() -> BarScript? {
            defer { barScript = nil }
            return barScript
        }

        /// Boot onto the fixture mailbox, then raise the scene asked for.
        static func boot(_ store: AppStore) async {
            if calendar { await RehearsalAPI.shared.stage(timeline: FixtureCalendar.feed()) }
            await store.enterPractice()
            await store.warmPractice()
            guard let scene else { return }
            // The shell mounts on the connect; its stacks have to exist before
            // anything is pushed onto one.
            try? await Task.sleep(for: .milliseconds(900))
            stage(scene, store)
        }

        private static func stage(_ scene: String, _ store: AppStore) {
            let first = store.sitrep.standing.first
            switch scene {
            case "thread":
                if let first { store.openThread(first.thread_id, queue: store.sitrep.standing, from: .sitrep) }
            // THE READER'S REPLY SURFACES. `reply` is the composer open on a
            // one-message thread; `chain` is a three-message conversation
            // parked at its end; `chain-reply`, `chain-replyall`,
            // `chain-recipients` and `chain-swipe` are that conversation with
            // a composer open, its recipient fields open, or the newest
            // card's rail pulled out.
            case "reply":
                if let first {
                    store.openThread(first.thread_id, queue: store.sitrep.standing, from: .sitrep)
                    Task { await openReply(on: first.id, store) }
                }
            // MAIL IN FRENCH, which the practice inbox does not have: the
            // reader's translation strip. `translated` is an HTML message
            // shown in English, `translated-incomplete` one whose fact check
            // failed, `translated-pending` one still being written (it stays
            // pending, so the reader's backoff runs out), `translated-deferred`
            // archive mail the reader asks to translate on open (it stays
            // deferred under any read until the ask), and
            // `translated-all` the three in one thread, where the pending one
            // lands on the reader's third refetch.
            case let scene where scene.hasPrefix("translated"):
                let kind: RehearsalAPI.TranslationFixture.Scene =
                    switch scene {
                    case "translated-incomplete": .incomplete
                    case "translated-pending": .pending
                    case "translated-deferred": .deferred
                    case "translated-all": .all
                    default: .done
                    }
                if let first { Task { await stageTranslated(first, kind, store) } }
            case let chain where chain.hasPrefix("chain"):
                if let first { Task { await stageChain(first, scene: chain, store) } }
            // THE READER ON A READING STACK, and the checkmark's hold on it.
            // The practice inbox has one email per newsletter, so the stack is
            // the whole Reading zone handed over as one sender's pile: three
            // emails, which is all the reader asks of a stack.
            //   stack         the reader, first of three
            //   stack-hold    the ring, stopped two thirds closed
            //   stack-tap     a tap: done, and on to the second
            //   stack-cancel  a hold let go of early: nothing marked
            //   stack-clear   a hold seen through: all three done, reader gone
            case "stack", "stack-hold", "stack-tap", "stack-cancel", "stack-clear":
                let pile = store.zones.reading.flatMap(\.items)
                guard let top = pile.first else { break }
                switch scene {
                case "stack-hold":
                    pinnedHold = 0.66
                    barScript = .pinned
                case "stack-tap": barScript = .press(held: .milliseconds(60))
                case "stack-cancel": barScript = .press(held: .milliseconds(600))
                case "stack-clear": barScript = .press(held: nil)
                default: break
                }
                store.openThread(top.thread_id, queue: pile, readingMail: true, from: .reading)
            case "remind":
                if let first {
                    store.openRemind(
                        RemindTarget(
                            messageId: first.id, sender: first.sender, subject: first.one_line,
                            remindAt: first.remind_at))
                }
            case "compose":
                store.openComposeNew()
            // THE DEEPER-SEARCH BAND on the phone's search tab (launch with
            // `--fixture-tab search`). "thursday?" is question-shaped by its
            // "?" and is also a literal substring of Alex's "Dinner on
            // Thursday?", which is all the practice search matches on, so the
            // band sits over one real hit. The practice API sends no
            // diagnostics, so the no-strict-hits trigger cannot be staged here.
            // SEARCH HISTORY with a question in it: wants `--fixture-tab search`.
            // `recents-restore` also keeps a chat behind the question and taps
            // it (MobileSearchView plays the tap: the push is its own state).
            case "recents", "recents-restore":
                RecentSearchStore.shared.record("delta receipt")
                RecentSearchStore.shared.rememberQuestion(Self.historyQuestion)
                RecentSearchStore.shared.record("from:maya@studio.example")
                if scene == "recents-restore" {
                    QuestionChats.shared.save(
                        Self.historyChat, forKey: QuestionChats.key(forQuestion: Self.historyQuestion))
                }
            case "search-lane":
                store.search.query = "thursday?"
            case "accounts":
                AccountDoor.shared.pane = .picker
            // THE SENT PAGE, which the practice mailbox leaves empty: nothing
            // in a rehearsal sends. `sent` is the list with an outbox pinned
            // above it (one mail waiting, one that failed); `sent-plain` is
            // the same list with nothing scheduled. Wants `--fixture-tab mail`.
            case "sent", "sent-plain":
                Task { await stageSent(outbox: scene == "sent", store) }
            case "tune":
                if let first { Actions.tune(sender: first.sender) }
            case "disconnected":
                // Lasts until the poller's next pull, which is long enough.
                SitrepPoller.shared.stop()
                store.stats?.gmail = GmailHealth(
                    connected: false,
                    disconnected_since: ISO8601DateFormatter().string(
                        from: Date().addingTimeInterval(-3 * 3600)),
                    reconnect_url: "https://example.com/reconnect")
            case "reconnect":
                SitrepPoller.shared.stop()
                store.stats?.gmail = GmailHealth(
                    connected: false, reconnect_url: "https://example.com/reconnect")
                // Straight to the prompt: the fixture has no account address,
                // and without one `askReconnect` opens the browser instead.
                store.reconnectAsk = AppStore.ReconnectAsk(
                    url: "https://example.com/reconnect", email: "jamie@example.com")
            case "violation":
                UnsubscribeViolations.shared.stage(
                    UnsubscribeRecord(
                        sender: "deals@brightly.example", requested_at: ISO8601DateFormatter()
                            .string(from: Date().addingTimeInterval(-9 * 86400)),
                        method: .oneClick, violation_count: 4,
                        smart_block: SmartBlockProposal(
                            rule: "Only show receipts and account notices.",
                            keeps: "receipts", drops: "promotions")))
            // SMART CLEAR'S RECEIPT over the sitrep, and its full list as the
            // sheet the receipt's link raises. Staged straight onto the store:
            // the press itself refuses the practice inbox. The clock is an
            // hour long so a screenshot never races it.
            case "smart-clear", "smart-clear-list":
                store.smartClearReceipt = FixtureSmartClear.receipt()
                if scene == "smart-clear-list" { store.openSmartClearList() }
            // THE COMPOSER'S TRASH, asked: a message with words in it (an
            // empty one closes without asking), then the confirm.
            case "compose-discard":
                store.openComposeNew()
                store.compose?.subject = "Lunch on Thursday"
                Task {
                    try? await Task.sleep(for: .milliseconds(900))
                    store.askDiscardCompose()
                }
            case "clear":
                SitrepPoller.shared.stop()
                store.sitrep.standing = []
                store.zones.reading = []
            // THE WHAT'S NEW WALK, raised the way Settings' row raises it and
            // opened on a page: `whatsnew` is the first, `whatsnew-3` the
            // third, and a number past the end is the closing page. It shows
            // the newest phone note there is, whatever this build is running.
            case let walk where walk.hasPrefix("whatsnew"):
                let page = Int(walk.dropFirst("whatsnew-".count)) ?? 1
                store.whatsNew.stage(page: page - 1)
            // THE ACCOUNT PAGE AND WHAT HANGS OFF IT; see `FixtureRoute`.
            case let page where FixtureRoute.scenes.contains(page):
                Task { await FixtureRoute.shared.stage(page, store) }
            default:
                break
            }
        }

        /// Rows the fixture daemon is handed and then asked for the ordinary
        /// way, so the page on screen came through `refreshSent` like any
        /// other. Long recipients, a long subject, no subject, no snippet, a
        /// read receipt: each is a way a two-line row can go wrong.
        private static func stageSent(outbox: Bool, _ store: AppStore) async {
            func at(_ hours: Double) -> String {
                Date().addingTimeInterval(hours * 3600).ISO8601Format()
            }
            func sent(
                _ id: Int, _ to: String, _ subject: String, _ snippet: String, hours: Double,
                opens: Int = 0
            ) -> SentItem {
                SentItem(
                    id: 7000 + id, thread_id: "practice-1", to: to, subject: subject,
                    snippet: snippet, sent_at: at(-hours), opens: opens)
            }
            func scheduled(
                _ id: Int, _ to: String, _ subject: String, _ body: String, hours: Double,
                state: String = "scheduled", error: String? = nil, files: Int = 0
            ) -> ScheduledSendView {
                ScheduledSendView(
                    id: id, send_at: at(hours), state: state, attempts: error == nil ? 0 : 3,
                    last_error: error, to: to, subject: subject, body: body,
                    attachments: (0..<files).map {
                        OutboundAttachment(
                            id: $0 + 1, filename: "deck-\($0).pdf", mime: "application/pdf",
                            size: 48_000, content_id: "")
                    },
                    created_at: at(-2), updated_at: at(-2))
            }
            await RehearsalAPI.shared.stage(
                sent: [
                    sent(
                        1, "Maya Chen <maya@studio.example>", "Re: Thursday, and the deck",
                        "Thursday works. Can you send the deck over before then?", hours: 0.4,
                        opens: 2),
                    sent(
                        2,
                        "Dana Okafor <dana@northwind.example>, Li Wen <li@harborvc.example>, "
                            + "Sam Ortiz <sam@fieldnotes.example>, priya@lanternfund.example",
                        "September update: revenue, the pilot renewal, and what we are hiring for next",
                        "Revenue is up 18% and the pilot renewed for another year.", hours: 5,
                        opens: 4),
                    sent(
                        3, "billing@harbor.example", "Question about the August statement",
                        "There is a charge on the 14th that I do not recognise. Could you tell me "
                            + "what it was for?", hours: 27),
                    sent(4, "Alex <alex@friends.example>", "", "Running ten minutes late, save me a seat", hours: 50),
                    sent(5, "returns@rainforest.example", "Return label for order 60218", "", hours: 75),
                    sent(
                        6, "Jo Lindqvist <jo@papermill.example>", "Intro: Dana and Maya",
                        "You two should talk about the studio. Dana, Maya runs the place I mentioned.",
                        hours: 200, opens: 1),
                ],
                scheduled: outbox
                    ? [
                        scheduled(
                            1, "Maya Chen <maya@studio.example>, alex@friends.example",
                            "Notes from the planning session, with next steps for each of us",
                            "Here is everything we agreed on, in the order we agreed on it.",
                            hours: 3, files: 2),
                        scheduled(
                            2, "events@citylibrary.example", "Two more seats for the author talk",
                            "Could you add two seats to my registration?", hours: -1,
                            state: "failed", error: "failed:gmail"),
                    ] : [])
            store.stats?.scheduled_send = outbox
            store.mailMode = .sent
            await store.refreshSent(force: true)
        }

        /// The question in the staged search history.
        static let historyQuestion = "when does my flight to Denver leave"

        /// The question to tap once its chat is saved (`recents-restore`).
        static var tapsHistoryQuestion: String? {
            scene == "recents-restore" ? historyQuestion : nil
        }

        /// A finished conversation for that question, as a run would have left
        /// it: a search, a read, an answer with a card, and a follow-up.
        private static var historyChat: ConversationSnapshot {
            let card = EmailCard(
                threadId: "t-flight", subject: "Your trip to Denver: confirmation K7PQ2M",
                sender: "United Airlines", date: "Oct 24",
                snippet: "UA 1423 departs SFO Thu Oct 30 at 7:05 AM, arrives DEN 10:38 AM.")
            let items: [ChatItem] = [
                ChatItem(id: 1, kind: .user, text: historyQuestion),
                ChatItem(
                    id: 2, kind: .tool,
                    tool: ToolActivity(
                        useId: "tu1", name: "search_mail", summary: "Searched \"flight Denver\"",
                        state: .ok)),
                ChatItem(id: 3, kind: .emails, emails: [card]),
                ChatItem(
                    id: 4, kind: .assistant,
                    text: "Thursday, October 30 at 7:05 AM from SFO (UA 1423)."),
                ChatItem(id: 5, kind: .user, text: "and the return?"),
                ChatItem(
                    id: 6, kind: .assistant,
                    text: "Sunday, November 2 at 6:20 PM from DEN, landing SFO at 8:01 PM."),
            ]
            let history: [Wire.MessageParam] = [
                .init(role: "user", content: .text(historyQuestion)),
                .init(
                    role: "assistant",
                    content: .blocks([
                        .toolUse(id: "tu1", name: "search_mail", input: ["query": .string("flight Denver")])
                    ])),
                .init(
                    role: "user",
                    content: .blocks([.toolResult(toolUseId: "tu1", content: "[]", isError: false)])),
                .init(role: "assistant", content: .blocks([.text(items[3].text)])),
                .init(role: "user", content: .text("and the return?")),
                .init(role: "assistant", content: .blocks([.text(items[5].text)])),
            ]
            return ConversationSnapshot(
                transcript: items, history: history, askedThreadIds: [nil, nil], nextItemId: 6)
        }

        /// The newest card's rail, pulled out for a screenshot (`chain-swipe`).
        static var railOut: Bool { scene == "chain-swipe" }
        /// The composer's recipient fields, already open (`chain-recipients`).
        static var recipientsOpen: Bool { scene == "chain-recipients" }
        /// The first contact card the reader mounts, opened by itself
        /// (`chain-contact`). Handed out ONCE: the sender's name, avatar and
        /// every recipient each carry a card, and only one can be up.
        private static var contactCardOwed = scene == "chain-contact"
        static func takeContactCard() -> Bool {
            defer { contactCardOwed = false }
            return contactCardOwed
        }

        /// Every door the composer has, open: the fixture daemon advertises
        /// none of them, and a composer staged without its paperclip, clock
        /// and tracker switch is not the composer anybody ships.
        private static func openReply(on id: Int, replyAll: Bool = false, _ store: AppStore) async {
            try? await Task.sleep(for: .milliseconds(1400))
            store.stats?.compose_attachments = true
            store.stats?.scheduled_send = true
            store.tracking = TrackingConfig(default_enabled: false, configured: true)
            store.openInlineReply(replyTo: id, replyAll: replyAll)
            guard recipientsOpen else { return }
            // The fixture daemon derives no audience, so the fields are handed
            // one the way a real derivation would have.
            try? await Task.sleep(for: .milliseconds(300))
            let set = Recipients(to: "maya@studio.example", cc: "alex@friends.example")
            store.inlineReply?.recipients = set
            store.inlineReply?.seededRecipients = set
            store.inlineReply?.recipientsStated = true
        }

        private static func longChain(
            opener: ClientMessage, mine: ClientMessage, theirs: ClientMessage
        ) -> [ClientMessage] {
            let paragraph =
                "<p>Picking this back up with the notes from today. The vendor confirmed the "
                + "revised timeline, the budget line moved into the second quarter, and the "
                + "review is now on the calendar for the week after next.</p>"
            return (0..<30).map { i in
                var m = i.isMultiple(of: 2) ? theirs : mine
                m.id = 9100 + i
                if i.isMultiple(of: 3) {
                    m = opener
                    m.id = 9100 + i
                    let count = [2, 24, 6, 40, 3][i / 3 % 5]
                    m.html = "<p>Round \(i + 1).</p>" + String(repeating: paragraph, count: count)
                } else {
                    m.content = "Message \(i + 1). " + m.content
                }
                return m
            }
        }

        /// A thread in French under a real practice thread's id, staged in the
        /// fixture daemon (so the reader's refetches see it too) and in the
        /// prefetch cache, in the same order `stageChain` explains.
        private static func stageTranslated(
            _ first: AttentionUpdate, _ kind: RehearsalAPI.TranslationFixture.Scene,
            _ store: AppStore
        ) async {
            let versions = RehearsalAPI.TranslationFixture.versions(kind, threadId: first.thread_id)
            guard let opening = versions.first else { return }
            await RehearsalAPI.shared.stage(thread: versions)
            store.openThread(first.thread_id, queue: store.sitrep.standing, from: .sitrep)
            ThreadPrefetch.shared.note(first.thread_id, opening)
        }

        /// A CONVERSATION, which the practice mailbox does not have: every
        /// fixture thread is one message. Built from the real one and two
        /// invented replies, and left in the prefetch cache where the reader's
        /// load looks first. Nothing is written to the fixture daemon.
        private static func stageChain(
            _ first: AttentionUpdate, scene: String, _ store: AppStore
        ) async {
            guard let real = try? await APIClient.shared.getThread(first.thread_id),
                var opener = real.messages.first
            else { return }
            opener.to_addrs = "you@example.com"
            // One web frame in the chain, because that is what most mail is,
            // and a rail has to pull out from over one.
            if opener.html == nil {
                opener.html =
                    "<p>Hey Jamie,</p><p>A quick recap so you have everything in one place. "
                    + "We are going with the quieter opening, keeping the product screenshots, "
                    + "and moving the longer background section to the appendix.</p>"
                    + "<p>I have updated the <a href=\"https://example.com/draft\">shared "
                    + "draft</a> to match. Please read it when you have a moment.</p>"
                    + "<p>Thanks!<br>Maya</p>"
            }
            var mine = opener
            mine.id = 9001
            mine.is_sent = true
            mine.from_addr = "you@example.com"
            mine.from_name = "You"
            mine.html = nil
            mine.content = "Thursday works. Can you send the deck over before then?"
            mine.to_addrs = opener.from_addr
            mine.attention_open = false
            var theirs = opener
            theirs.id = 9002
            theirs.html = nil
            theirs.content =
                "Sending it tonight. Alex is joining too, so I have copied him in.\n\n"
                + "Let me know if the afternoon slot is still good for you."
            theirs.cc_addrs = "alex@friends.example"
            theirs.attention_open = false
            // OPENED FIRST, CACHED SECOND, in one turn of the run loop: the
            // store seeds a reader from the cache at the moment it opens, and
            // a seeded reader is a different (and rarer) path than the load
            // every practice thread takes. This way the reader mounts empty,
            // and its own load finds the conversation waiting.
            // `chain-long`: THIRTY MESSAGES, the long thread a phone has to
            // land on and hold while every card above it measures. Every
            // third one is a long html email, so the guesses are wrong by
            // thousands of points, which is what a real chain does.
            let messages =
                scene == "chain-long"
                ? longChain(opener: opener, mine: mine, theirs: theirs) : [opener, mine, theirs]
            store.openThread(first.thread_id, queue: store.sitrep.standing, from: .sitrep)
            ThreadPrefetch.shared.note(
                first.thread_id,
                ClientThreadView(
                    cache_allowed: true, thread_id: real.thread_id, subject: real.subject,
                    messages: messages))
            switch scene {
            case "chain-reply", "chain-recipients": await openReply(on: 9002, store)
            case "chain-replyall": await openReply(on: 9002, replyAll: true, store)
            default: break
            }
        }
    }

    /// THE PUSHES NOTHING CAN TAP. The Account page and the Rules and Groups
    /// pages under it are reached through navigation links, and a link has no
    /// state for a launch argument to set. So each of those screens watches
    /// one field here, in a fenced block of its own, and pushes when it fills.
    ///
    ///   account        the Account index
    ///   rules          the Rules page, with rules across all three outcomes
    ///   rules-empty    the same page on a mailbox with none
    ///   rule-new       the + button: the editor on a blank rule
    ///   rule-edit      a row tapped: the editor on that rule
    ///   groups         the Groups page, three groups
    ///   groups-empty   the same page with none
    ///   group          a row tapped: one group's people and history
    ///   group-new      the + button: the editor on a blank group
    ///   group-edit     edit, from the detail: the editor with its people
    ///   group-thread   a history row tapped: the reader, over the detail
    ///   group-back     that reader closed again: the detail it came from
    ///   group-crowd    the editor on a group too big for the screen
    ///   general        the General pane, which ends in the What's new row
    ///   agents         the Agents pane, on a door nobody has probed (the
    ///                  fixture daemon has none), which shows the setup card
    ///
    /// The editors are raised with the store call the views' own verbs end in
    /// (`openRuleEditor`, `openGroupEditor`), not through those verbs, which
    /// are private to their views. That is the one way a staged screen differs
    /// from a tapped one: the card is the real card, but nobody pressed the
    /// real button, so the request carries no `onSaved`.
    @MainActor @Observable
    final class FixtureRoute {
        static let shared = FixtureRoute()

        enum Page: String, Hashable, Identifiable {
            case rules, groups, general, agents
            var id: String { rawValue }
        }

        static let scenes: Set<String> = [
            "account", "rules", "rules-empty", "rule-new", "rule-edit",
            "groups", "groups-empty", "group", "group-new", "group-edit", "group-thread",
            "group-back", "group-crowd", "group-swap", "general", "agents",
        ]

        /// Sitrep to Account. Watched by the sitrep tab's stack (MobileRootView).
        var account = false
        /// Account to one of its two pages. Watched by AccountPage.
        var page: Page?
        /// Groups to one group's detail. Watched by GroupsView.
        var group: Int?

        func stage(_ scene: String, _ store: AppStore) async {
            if !scene.hasSuffix("-empty"), scene != "general", scene != "agents" {
                await RehearsalAPI.shared.stage(
                    rules: Self.rules, groups: Self.groups, history: Self.history)
                // The match counts on the Rules page read the board, and the
                // board was pulled before these rules existed.
                await SitrepPoller.shared.pull()
            }
            account = true
            guard scene != "account" else { return }
            // One push at a time: a stack handed two in the same pass drops
            // the second.
            try? await Task.sleep(for: .milliseconds(700))
            page =
                scene == "general"
                ? .general
                : scene == "agents" ? .agents : scene.hasPrefix("rule") ? .rules : .groups
            try? await Task.sleep(for: .milliseconds(700))
            switch scene {
            case "rule-new":
                store.openRuleEditor(RuleEditorRequest(rule: nil))
            case "rule-edit":
                store.openRuleEditor(RuleEditorRequest(rule: Self.rules[3]))
            case "group-new":
                store.openGroupEditor(GroupEditorRequest(group: nil))
            case "group", "group-edit", "group-thread", "group-back", "group-crowd", "group-swap":
                group = 1
                try? await Task.sleep(for: .milliseconds(700))
                switch scene {
                case "group-edit":
                    store.openGroupEditor(GroupEditorRequest(group: Self.groups[0]))
                case "group-crowd":
                    var crowd = Self.groups[0]
                    crowd.members = (1...14).map {
                        GroupMember(addr: "partner\($0)@fund\($0).example", display_name: nil)
                    }
                    store.openGroupEditor(GroupEditorRequest(group: crowd))
                case "group-swap":
                    // A second thread arriving over the first (a banner tap):
                    // the reader should end up on the second, over the group.
                    store.openThread("practice-1", from: .groups)
                    try? await Task.sleep(for: .milliseconds(1500))
                    store.openThread("practice-2", from: .groups)
                case "group-thread", "group-back":
                    store.openThread("practice-1", from: .groups)
                    guard scene == "group-back" else { break }
                    try? await Task.sleep(for: .milliseconds(1500))
                    store.closeThread()
                default:
                    break
                }
            default:
                break
            }
        }

        private static func ago(_ days: Double) -> String {
            Date().addingTimeInterval(-days * 86400).ISO8601Format()
        }

        private static func makeRule(
            _ id: Int, _ pattern: String, _ disposition: Disposition, _ want: String, days: Double
        ) -> SenderRule {
            SenderRule(
                id: id, account_id: 1, match_pattern: pattern, want_text: want,
                disposition: disposition, updated_at: ago(days))
        }

        /// Written against the practice inbox's own senders, so some of them
        /// match mail that is on the board. One domain carries two rules (the
        /// nested header), one rule has no sentence, and one sentence is long
        /// enough to wrap.
        static let rules: [SenderRule] = [
            makeRule(1, "maya@studio.example", .surface, "always show me anything from Maya", days: 0.2),
            makeRule(2, "*@harbor.example", .surface, "statements and anything about my account", days: 12),
            makeRule(3, "alex@friends.example", .surface, "", days: 40),
            makeRule(
                4, "*@rainforest.example", .filtered,
                "only shipping updates and delivery problems. no deals, no recommendations, no \"you might also like\".",
                days: 3),
            makeRule(
                5, "returns@rainforest.example", .filtered,
                "tell me when a refund actually lands", days: 3),
            makeRule(
                6, "events@citylibrary.example", .filtered,
                "only holds that are ready and due dates", days: 21),
            makeRule(7, "*@brightly.example", .squelch, "i never want marketing from these people", days: 9),
            makeRule(8, "edition@federaloverstatement.example", .squelch, "not interested anymore", days: 60),
            makeRule(9, "no-reply@zip.example", .squelch, "", days: 2),
        ]

        private static func people(_ pairs: [(String, String?)]) -> [GroupMember] {
            pairs.map { GroupMember(addr: $0.0, display_name: $0.1) }
        }

        private static func makeGroup(
            _ id: Int, _ name: String, _ mode: GroupMode, note: String, lastSent: Double?,
            _ members: [GroupMember]
        ) -> SendGroup {
            SendGroup(
                id: id, name: name, slug: name.replacingOccurrences(of: " ", with: "-"), mode: mode,
                note: note, member_count: members.count, members: members,
                last_sent_at: lastSent.map(ago), created_at: ago(200), updated_at: ago(30))
        }

        static let groups: [SendGroup] = [
            makeGroup(
                1, "preseed investors", .individual, note: "monthly update, first tuesday",
                lastSent: 6,
                people([
                    ("dana@northwind.example", "Dana Okafor"), ("li@harborvc.example", "Li Wen"),
                    ("sam@fieldnotes.example", "Sam Ortiz"), ("priya@lanternfund.example", "Priya Rao"),
                    ("tom@quietcapital.example", nil),
                ])),
            makeGroup(
                2, "design partners", .bcc, note: "", lastSent: 20,
                people([
                    ("maya@studio.example", "Maya Chen"), ("jo@papermill.example", "Jo Lindqvist"),
                    ("ops@cornercoffee.example", nil),
                ])),
            makeGroup(
                3, "family", .to, note: "", lastSent: nil,
                people([
                    ("alex@friends.example", "Alex"), ("mum@home.example", "Mum"),
                    ("ines@home.example", "Ines"), ("theo@home.example", "Theo"),
                ])),
        ]

        /// A send that reached everyone, one that did not, and one piece of
        /// ordinary mail matched to the membership afterwards.
        static let history: [Int: [GroupHistoryEntry]] = [
            1: [
                GroupHistoryEntry(
                    group_send_id: 12, message_id: nil, thread_id: "practice-1",
                    subject: "September update", snippet: "Revenue is up 18% and the pilot renewed.",
                    sent_at: ago(6), mode: .individual, reached: 5, group_size: 5, failed: 0, opens: 4),
                GroupHistoryEntry(
                    group_send_id: 9, message_id: nil, thread_id: "practice-1",
                    subject: "August update", snippet: "A slower month, and why that is fine.",
                    sent_at: ago(37), mode: .individual, reached: 4, group_size: 5, failed: 1, opens: 3),
                GroupHistoryEntry(
                    group_send_id: nil, message_id: 1, thread_id: "practice-1",
                    subject: "Intro: Dana and Maya", snippet: "You two should talk about the studio.",
                    sent_at: ago(64), mode: .to, reached: 1, group_size: 5, failed: 0, opens: 0),
            ],
            2: [
                GroupHistoryEntry(
                    group_send_id: 4, message_id: nil, thread_id: "practice-1",
                    subject: "The new onboarding, for your eyes first", snippet: "Three screens, attached.",
                    sent_at: ago(20), mode: .bcc, reached: 3, group_size: 3, failed: 0, opens: 2),
            ],
        ]
    }
#endif

#if DEBUG
    /// The receipt `smart-clear` stages: a batch with an undo, across the
    /// reasons the copy has words for, with rows for the full list.
    enum FixtureSmartClear {
        @MainActor
        static func receipt() -> AppStore.SmartClearReceipt {
            let now = Date()
            func ago(_ days: Double) -> String {
                ISO8601DateFormatter().string(from: now.addingTimeInterval(-days * 86400))
            }
            let items = [
                SmartClearItem(
                    id: 91001, reason: "event_passed", sender: "events@citylights.example",
                    senderName: "City Lights", subject: "Last call: rooftop film night", receivedAt: ago(9)),
                SmartClearItem(
                    id: 91002, reason: "offer_ended", sender: "deals@brightly.example",
                    senderName: "Brightly", subject: "48 hours only: 30% off everything", receivedAt: ago(12)),
                SmartClearItem(
                    id: 91003, reason: "offer_ended", sender: "hello@northwind.example",
                    senderName: "Northwind", subject: "Your weekend code expires Sunday", receivedAt: ago(16)),
                SmartClearItem(
                    id: 91004, reason: "stale_promo", sender: "news@paperco.example",
                    senderName: "PaperCo", subject: "New arrivals for fall", receivedAt: ago(21)),
                SmartClearItem(
                    id: 91005, reason: "stale_promo", sender: "news@paperco.example",
                    senderName: "PaperCo", subject: "The notebook edit", receivedAt: ago(28)),
                SmartClearItem(
                    id: 91006, reason: "ignored_sender", sender: "digest@forum.example",
                    senderName: "", subject: "Your weekly forum digest", receivedAt: ago(5)),
            ]
            var byReason: [String: Int] = [:]
            for item in items { byReason[item.reason, default: 0] += 1 }
            return AppStore.SmartClearReceipt(
                batchId: "fixture", cleared: items.count, senders: 5, byReason: byReason,
                messageIds: items.map(\.id), modelNote: nil, createdAt: now,
                clock: SmartClearClock(startedAt: now, lasting: 3600), items: items)
        }
    }
#endif

#if DEBUG
    import Foundation

    /// A week of plans around today, in the device's zone: real events on two
    /// calendars, and ghosts from mail in every band, so a screenshot shows
    /// each fill, a guessed end, a trip across days, a fold and a cancellation.
    enum FixtureCalendar {
        static func feed(now: Date = Date()) -> TimelineFeed {
            let cal = Calendar.current
            let today = cal.startOfDay(for: now)
            let iso = ISO8601DateFormatter()
            iso.timeZone = cal.timeZone
            iso.formatOptions = [.withInternetDateTime]
            func at(_ day: Int, _ hour: Int, _ minute: Int = 0) -> String {
                let d = cal.date(byAdding: .day, value: day, to: today)!
                return iso.string(from: cal.date(bySettingHour: hour, minute: minute, second: 0, of: d)!)
            }
            func date(_ day: Int) -> String {
                let d = cal.date(byAdding: .day, value: day, to: today)!
                let c = cal.dateComponents([.year, .month, .day], from: d)
                return String(format: "%04d-%02d-%02d", c.year!, c.month!, c.day!)
            }
            let zone = cal.timeZone.identifier
            let work = "#4285F4", home = "#33B679", family = "#E67C73"
            func event(
                _ id: String, _ title: String, _ start: String, _ end: String?, allDay: Bool = false,
                calendar: String = "Work", color: String = work, place: String? = nil,
                response: TimelineSelfResponse? = nil, guests: [TimelineGuest]? = nil
            ) -> TimelineEvent {
                TimelineEvent(
                    id: id, calendar_id: calendar.lowercased(), calendar_name: calendar, title: title,
                    start: start, end: end, all_day: allDay, start_timezone: zone, location: place,
                    status: .confirmed, self_response: response, busy: true, organizer: nil,
                    html_link: "https://calendar.google.com/", color: color, editable: true,
                    description: nil, guests: guests, guests_truncated: false, guest_editable: true)
            }
            func ghost(
                _ id: Int, _ kind: GhostKind, _ title: String, _ start: String, end: String? = nil,
                allDay: Bool = false, estimated: Bool = false, place: String? = nil,
                commitment: GhostCommitment, state: GhostState = .ghost, mail: GhostMailState = .live,
                band: GhostBand? = nil, reason: String? = nil, message: Int = 3
            ) -> TimelineGhost {
                TimelineGhost(
                    id: id, kind: kind, title: title, start: start, start_timezone: zone,
                    all_day: allDay, end: end, end_estimated: estimated,
                    end_source: estimated ? .model : (end == nil ? nil : .stated),
                    estimate: estimated ? GhostEstimate(minutes: 120, basis: "dinner for four at 7:30") : nil,
                    location: place, commitment: commitment, state: state, mail_state: mail,
                    verified: true,
                    likelihood: band.map { GhostLikelihood(band: $0, reason: reason) },
                    sources: [GhostSource(message_id: message, thread_id: "practice-\(message)", role: .created)],
                    matched_event: nil)
            }
            let events = [
                event("e1", "Yoga", at(-2, 7, 30), at(-2, 8, 30), calendar: "Personal", color: home),
                event("e2", "Standup", at(0, 9, 30), at(0, 9, 45)),
                event("e3", "Lunch with Priya", at(0, 12, 30), at(0, 13, 30), calendar: "Personal", color: home,
                      place: "Tartine Manufactory"),
                event("e4", "Design review", at(0, 16), at(0, 17), place: "Room 4B", response: .needsAction,
                      guests: [
                          TimelineGuest(email: "sam@studio.example", name: "Sam Ortiz", response: .accepted, organizer: true),
                          TimelineGuest(email: "jamie@example.com", name: "Jamie", response: .pending, isSelf: true),
                      ]),
                event("e5", "Mom's birthday", date(1), date(1), allDay: true, calendar: "Family", color: family),
                event("e6", "1:1 with Sam", at(1, 10), at(1, 10, 30)),
                event("e7", "Dinner with the Denver team", at(3, 19), at(3, 21), place: "Tavernetta"),
                event("e8", "Dentist", at(9, 8, 30), at(9, 9, 30), calendar: "Personal", color: home),
            ]
            let ghosts = [
                ghost(1, .reservation, "Dinner at Nopa", at(0, 19, 30), end: at(0, 21, 30), estimated: true,
                      place: "Nopa, 560 Divisadero", commitment: .committed, message: 4),
                ghost(2, .invite, "Climbing at Mission Cliffs", at(1, 18), place: "Mission Cliffs",
                      commitment: .invited, band: .likely, reason: "they asked twice and you said probably"),
                ghost(3, .invite, "Book club", at(1, 20), commitment: .invited, band: .maybe),
                ghost(4, .travel, "Denver trip", date(2), end: date(4), allDay: true, commitment: .committed),
                ghost(5, .travel, "UA 1532 SFO to DEN", at(2, 7, 5), end: at(2, 10, 40),
                      place: "SFO Terminal 3", commitment: .committed, message: 6),
                ghost(6, .invite, "Founders mixer", at(3, 18), commitment: .invited, band: .unlikely),
                ghost(7, .proposal, "Coffee with Alex", at(4, 9), commitment: .proposed,
                      state: .cancelled, mail: .cancelled, band: .likely),
                ghost(8, .ticket, "Japanese Breakfast", at(6, 20), end: at(6, 23), estimated: true,
                      place: "The Fillmore", commitment: .committed, message: 7),
            ]
            return TimelineFeed(
                ghosts: ghosts, events: events,
                calendar: TimelineCalendarStatus(granted: true, synced_at: iso.string(from: now)),
                calendars: [
                    TimelineCalendar(id: "work", name: "Work", color: work, primary: true, writable: true),
                    TimelineCalendar(id: "personal", name: "Personal", color: home, primary: false, writable: true),
                ])
        }
    }
#endif
