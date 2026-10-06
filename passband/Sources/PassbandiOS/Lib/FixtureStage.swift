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

        /// `sitrep` (the default), `records`, `mail` or `search`.
        static var tab: String? { value("--fixture-tab") }
        /// What to raise once the board has loaded; see `stage`.
        static var scene: String? { value("--fixture-scene") }

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
            case "clear":
                SitrepPoller.shared.stop()
                store.sitrep.standing = []
                store.zones.reading = []
            // THE ACCOUNT PAGE AND WHAT HANGS OFF IT; see `FixtureRoute`.
            case let page where FixtureRoute.scenes.contains(page):
                Task { await FixtureRoute.shared.stage(page, store) }
            default:
                break
            }
        }

        /// The newest card's rail, pulled out for a screenshot (`chain-swipe`).
        static var railOut: Bool { scene == "chain-swipe" }
        /// The composer's recipient fields, already open (`chain-recipients`).
        static var recipientsOpen: Bool { scene == "chain-recipients" }

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
            store.openThread(first.thread_id, queue: store.sitrep.standing, from: .sitrep)
            ThreadPrefetch.shared.note(
                first.thread_id,
                ClientThreadView(
                    cache_allowed: true, thread_id: real.thread_id, subject: real.subject,
                    messages: [opener, mine, theirs]))
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
            case rules, groups
            var id: String { rawValue }
        }

        static let scenes: Set<String> = [
            "account", "rules", "rules-empty", "rule-new", "rule-edit",
            "groups", "groups-empty", "group", "group-new", "group-edit", "group-thread",
            "group-back", "group-crowd",
        ]

        /// Sitrep to Account. Watched by the sitrep tab's stack (MobileRootView).
        var account = false
        /// Account to one of its two pages. Watched by AccountPage.
        var page: Page?
        /// Groups to one group's detail. Watched by GroupsView.
        var group: Int?

        func stage(_ scene: String, _ store: AppStore) async {
            if !scene.hasSuffix("-empty") {
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
            page = scene.hasPrefix("rule") ? .rules : .groups
            try? await Task.sleep(for: .milliseconds(700))
            switch scene {
            case "rule-new":
                store.openRuleEditor(RuleEditorRequest(rule: nil))
            case "rule-edit":
                store.openRuleEditor(RuleEditorRequest(rule: Self.rules[3]))
            case "group-new":
                store.openGroupEditor(GroupEditorRequest(group: nil))
            case "group", "group-edit", "group-thread", "group-back", "group-crowd":
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
